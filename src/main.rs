use std::{
    collections::HashMap,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
};

use axum::{
    Router,
    routing::{get, post},
};
use clap::{Parser, Subcommand};
use notify_debouncer_full::{
    new_debouncer,
    notify::{EventKind, RecursiveMode},
};
use tracing::{info, warn};
use tracing_subscriber::{
    EnvFilter, Layer, fmt::time::LocalTime, layer::SubscriberExt, util::SubscriberInitExt,
};
use tymnl::{
    config::{Config, ScreenOption},
    render::{self, Depth, Renderer},
};

use state::AppState;

mod handlers;
mod state;

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Parser)]
#[command(name = "tymnl", version = env!("TYMNL_VERSION"))]
struct Args {
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Path to the config file
    #[arg(short = 'c', long = "config", global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the tyMNL server
    Serve {
        /// Address to listen on
        #[arg(short = 'l', long = "listen", default_value = "0.0.0.0")]
        host: String,
        /// Port to listen on
        #[arg(short = 'p', long = "port", default_value_t = 3003)]
        port: u16,
        /// Disable hot-reloading of the config file (useful on filesystems without inotify)
        #[arg(long = "no-reload", default_value_t = false)]
        no_reload: bool,
        /// Disable timestamps in log output (useful when logs are captured by syslog/journald)
        #[arg(long = "no-timestamps", default_value_t = false)]
        no_timestamps: bool,
    },
    /// Render a screen and write the PNG to stdout (or out.png)
    Show {
        /// Name of the screen to render
        screen: Option<String>,
        /// List all available screens
        #[arg(short = 'l', long = "list")]
        list: bool,
    },
    /// List all fonts loaded into the renderer
    Fonts,
    /// Print the built-in tymnl.typ template to stdout
    Template,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let debug = if args.verbose { "debug" } else { "info" };
    let no_timestamps = matches!(
        args.command,
        Command::Serve {
            no_timestamps: true,
            ..
        }
    );

    let fmt_layer = if no_timestamps {
        tracing_subscriber::fmt::layer().without_time().boxed()
    } else {
        tracing_subscriber::fmt::layer()
            .with_timer(LocalTime::rfc_3339())
            .boxed()
    };

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(EnvFilter::new(format!("tymnl={},axum=warn", debug)))
        .init();

    let config_path = args.config.unwrap_or_else(|| {
        let home = std::env::var("HOME").expect("HOME not set");
        PathBuf::from(home).join(".config/tymnl/tymnl.yml")
    });

    if let Err(e) = run(args.command, config_path).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

async fn run(command: Command, config_path: PathBuf) -> Result<(), Error> {
    match command {
        Command::Serve {
            host,
            port,
            no_reload,
            ..
        } => serve(config_path, host, port, no_reload).await?,
        Command::Show { screen, list } => {
            if list {
                let (config, _) = Config::load(&config_path)?;
                for s in &config.screen {
                    println!("{}", s.name);
                }
            } else {
                let screen_name = screen.ok_or("screen name required")?;
                show(config_path, screen_name).await?;
            }
        }
        Command::Template => print!("{}", include_str!("templates/tymnl.typ")),
        Command::Fonts => {
            let (_, config_dir) = Config::load(&config_path)?;
            let renderer = render::Renderer::new(config_dir)?;
            let mut names: Vec<_> = renderer
                .fonts()
                .iter()
                .map(|f| f.info().family.clone())
                .collect();
            names.sort();
            names.dedup();
            for name in names {
                println!("{}", name);
            }
        }
    }
    Ok(())
}

async fn serve(
    config_path: PathBuf,
    host: String,
    port: u16,
    no_reload: bool,
) -> Result<(), Error> {
    let (config, config_dir) = Config::load(&config_path)?;

    let state = Arc::new(AppState {
        config: RwLock::new(config),
        renderer: Renderer::new(config_dir)?,
        playlist_indices: Mutex::new(HashMap::new()),
        next_screens: Mutex::new(HashMap::new()),
    });

    if !no_reload {
        spawn_config_watcher(Arc::clone(&state), config_path.clone());
    }

    let app = Router::new()
        .route("/api/setup", get(handlers::setup))
        .route("/api/display", get(handlers::display))
        .route("/api/log", post(handlers::log))
        .route("/screen/{mac}/{file}", get(handlers::screen))
        .route("/error/{mac}/{file}", get(handlers::error))
        .route("/welcome/{mac}/{file}", get(handlers::welcome))
        .with_state(state);

    info!("Config: {:?}", config_path);
    info!("Endpoints:");
    info!("  GET  /api/setup   - Device registration");
    info!("  GET  /api/display - Get display image");
    info!("  POST /api/log     - Device telemetry");

    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| std::io::Error::new(e.kind(), format!("Failed to bind {addr}: {e}")))?;
    info!("Listening on http://{}", listener.local_addr().unwrap());
    axum::serve(listener, app).await?;
    Ok(())
}

async fn show(config_path: PathBuf, screen: String) -> Result<(), Error> {
    let (config, config_dir) = Config::load(&config_path)?;
    let screen_cfg = config
        .get_screen_by_name(&screen)
        .ok_or_else(|| format!("screen '{}' not found in config", screen))?;
    let renderer = render::Renderer::new(config_dir)?;

    let (inputs, _) = screen_cfg.query_inputs().await?;

    let bit_depth = if screen_cfg.option.contains(&ScreenOption::Grayscale) {
        Depth::Bit2
    } else {
        Depth::Bit1
    };

    let timezone: chrono_tz::Tz = config.timezone.parse().unwrap_or(chrono_tz::UTC);
    let then = std::time::Instant::now();
    let png = renderer.render(
        screen_cfg.script()?,
        Some(inputs),
        128.0,
        bit_depth,
        timezone,
    )?;
    eprintln!("Rendering took {:?}", then.elapsed());

    let stdout = io::stdout();
    if !stdout.is_terminal() {
        let mut stdout = stdout.lock();
        stdout.write_all(&png)?;
        stdout.flush()?;
    } else {
        eprintln!("Could not print PNG to stdout, saving to \"out.png\" instead");
        std::fs::write("out.png", &png)?;
    }
    Ok(())
}

fn spawn_config_watcher(state: Arc<state::AppState>, config_path: PathBuf) {
    let (tx, rx) = std::sync::mpsc::channel();

    let Some(watch_dir) = config_path.parent().map(|p| p.to_path_buf()) else {
        warn!("Config file has no parent directory, hot-reload disabled");
        return;
    };

    let mut debouncer =
        match new_debouncer(std::time::Duration::from_millis(200), None, move |result| {
            if let Ok(events) = result {
                let _ = tx.send(events);
            }
        }) {
            Ok(d) => d,
            Err(e) => {
                warn!("Failed to create file watcher, hot-reload disabled: {e}");
                return;
            }
        };

    if let Err(e) = debouncer.watch(&watch_dir, RecursiveMode::Recursive) {
        warn!("Failed to watch config directory, hot-reload disabled: {e}");
        return;
    }

    std::thread::spawn(move || {
        let _debouncer = debouncer; // keep debouncer alive

        for events in rx {
            let has_write = events
                .iter()
                .any(|e| matches!(e.kind, EventKind::Create(_) | EventKind::Modify(_)));
            if !has_write {
                continue;
            }
            match Config::load(&config_path) {
                Ok((new_config, _)) => {
                    *state.config.write().expect("config lock poisoned") = new_config;
                    info!("Config reloaded from {:?}", config_path);
                }
                Err(e) => {
                    warn!("Config reload failed, keeping old config: {e}");
                }
            }
        }
    });
}
