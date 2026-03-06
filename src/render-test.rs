use std::io::{self, IsTerminal, Write};

use tymnl::config::Config;
use tymnl::render::{self, Depth};

use clap::Parser;

#[derive(Parser)]
#[command(version, about = "Test rendering")]
struct Args {
    screen: String,
}

fn main() {
    let args = Args::parse();

    let (config, config_dir) =
        Config::load("example-config/tymnl.yml").expect("Loading config failed");
    let screen = config.get_screen_by_name(&args.screen).unwrap();
    let renderer = render::Renderer::new(config_dir).expect("Creating renderer failed");

    let rt = tokio::runtime::Runtime::new().unwrap();

    let (mut inputs, _) = rt.block_on(async { screen.query_inputs().await }).unwrap();
    inputs.insert("battery-percent".into(), "0".into());

    let timezone: chrono_tz::Tz = config.timezone.parse().unwrap_or(chrono_tz::UTC);
    let then = std::time::Instant::now();
    let png = renderer
        .render(
            screen.script().unwrap(),
            Some(inputs),
            128.0,
            Depth::Bit1,
            timezone,
        )
        .expect("Render failed");
    eprintln!("Rendering took {:?}", then.elapsed());

    let stdout = io::stdout();
    if !stdout.is_terminal() {
        let mut stdout = stdout.lock();
        stdout.write_all(&png).unwrap();
        stdout.flush().unwrap()
    } else {
        eprintln!("Could not print PNG to stdout, saving to \"out.png\" instead");
        std::fs::write("out.png", &png).unwrap()
    }
}
