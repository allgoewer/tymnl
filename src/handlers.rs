use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use axum_extra::{TypedHeader, headers::Host};
use chrono::Utc;
use chrono_tz::Tz;
use rand::prelude::RngExt;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};
use trmnl::{DeviceInfo, DisplayResponse, LogEntry, LogResponse, SetupResponse};
use tymnl::{
    config::{Inputs, Screen, ScreenOption},
    render::{self, Depth, Renderer},
};

fn forwarded_proto(headers: &HeaderMap) -> &str {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .unwrap_or("http")
}

use crate::state::{AppState, Error, NextScreen};

const DISPLAY_PPI: f32 = 128.0;
const ERROR_TEMPLATE: &str = include_str!("templates/error.typ");
const WELCOME_TEMPLATE: &str = include_str!("templates/welcome.typ");

// --- helpers -----------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
struct DeviceInputs {
    screen_name: String,
    battery_percentage: String,
    id: String,
    next_update_at: String,
}

impl DeviceInputs {
    fn new(screen_name: &str, battery_percentage: u8, id: &str, timezone: &str) -> Self {
        let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
        let next_update_at = Utc::now().with_timezone(&tz).format("%H:%M").to_string();

        Self {
            screen_name: screen_name.to_owned(),
            battery_percentage: battery_percentage.to_string(),
            id: id.to_owned(),
            next_update_at,
        }
    }

    fn insert_into(&self, map: &mut HashMap<String, String>) {
        map.insert(
            "tymnl-internal".to_owned(),
            serde_json::to_string(self).expect("Can't serialize DeviceInputs"),
        );
    }
}

fn calculate_battery_percentage(voltage: f32) -> u8 {
    let pct = -144.9390 * voltage * voltage * voltage + 1655.8629 * voltage * voltage
        - 6158.8520 * voltage
        + 7501.3202;
    pct.clamp(0.0, 100.0) as u8
}

async fn render_png(
    renderer: &Renderer,
    source: impl Into<String>,
    inputs: Option<HashMap<String, String>>,
    ppi: f32,
    bit_depth: Depth,
    timezone: Tz,
) -> Result<Vec<u8>, Error> {
    let renderer = renderer.clone();
    let source = source.into();

    let png = tokio::task::spawn_blocking(move || {
        renderer.render(source, inputs, ppi, bit_depth, timezone)
    })
    .await
    .expect("Thread did not join")?;

    Ok(png)
}

fn get_screen(state: &AppState, mac: &str) -> Result<Screen, Error> {
    let config = state.config();
    let playlist = config
        .get_active_playlist()
        .ok_or(Error::NoActivePlaylist)?;
    let index = state.next_playlist_index(mac);
    let screen = playlist
        .get_next_screen(&config, index)
        .ok_or_else(|| Error::NoScreen(playlist.name.clone()))?;
    Ok(screen.clone())
}

async fn query_inputs(
    screen: &Screen,
    device_inputs: DeviceInputs,
) -> Result<(Inputs, u64), Error> {
    let (mut inputs, hash) = screen.query_inputs().await?;

    device_inputs.insert_into(&mut inputs);

    Ok((inputs, hash))
}

fn special_screen_response(
    path: &str,
    mac: &str,
    next: NextScreen,
    state: &AppState,
    hostname: &str,
    refresh_rate: u32,
    protocol: &str,
) -> Json<DisplayResponse> {
    let random: u64 = rand::rng().random();

    state.put_next_screen(mac, next);

    Json(
        DisplayResponse::new(
            format!(
                "{}://{}/{}/{}/{}.png",
                protocol, hostname, path, mac, random
            ),
            format!("{}.png", random),
        )
        .with_refresh_rate(refresh_rate),
    )
}

fn image_response(
    mac: &str,
    next: NextScreen,
    state: &AppState,
    hostname: &str,
    hash: u64,
    refresh_rate: u32,
    protocol: &str,
) -> Json<DisplayResponse> {
    state.put_next_screen(mac, next);

    Json(
        DisplayResponse::new(
            format!(
                "{}://{}/screen/{}/{}.png",
                protocol, hostname, mac, hash
            ),
            format!("{}.png", hash),
        )
        .with_refresh_rate(refresh_rate),
    )
}

// --- handlers ----------------------------------------------------------------

/// GET /api/setup - Device registration
pub async fn setup(
    State(state): State<Arc<AppState>>,
    TypedHeader(host): TypedHeader<Host>,
    headers: HeaderMap,
    device: DeviceInfo,
) -> Json<SetupResponse> {
    info!("Device {} requesting setup", device.mac_address);

    let mac = &device.mac_address;
    let battery_pct = calculate_battery_percentage(device.battery_voltage.unwrap_or_default());
    let protocol = forwarded_proto(&headers);
    let timezone = state.config().timezone.clone();
    let mut inputs = HashMap::new();

    DeviceInputs::new("Setup", battery_pct, mac, &timezone).insert_into(&mut inputs);
    inputs.insert("mac".to_owned(), mac.to_owned());

    let random: u64 = rand::rng().random();

    state.put_next_screen(mac, NextScreen::new_welcome(inputs));

    let image_url = format!(
        "{}://{}/welcome/{}/{}.png",
        protocol,
        host.hostname(),
        mac,
        random
    );

    Json(SetupResponse::new(
        format!("trmnl-{}", device.short_id()),
        image_url,
        "Welcome to BYOS!",
    ))
}

/// GET /error/{mac}/*.png - Error page
pub async fn error(
    State(state): State<Arc<AppState>>,
    Path((mac, _)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    state.cleanup_old_screens();

    let timezone: Tz = state.config().timezone.parse().unwrap_or(Tz::UTC);

    let next = state
        .take_next_screen(&mac)
        .unwrap_or_else(|| NextScreen::new_error(Error::Unknown, HashMap::new()));

    let inputs = match next {
        NextScreen::Error { error, mut inputs } => {
            inputs.entry("error".to_owned()).or_insert_with(|| error.to_string());
            inputs
        }
        NextScreen::Image { inputs, .. } | NextScreen::Welcome { inputs } => inputs,
    };

    let png = render_png(
        &state.renderer,
        ERROR_TEMPLATE,
        Some(inputs),
        DISPLAY_PPI,
        Depth::Bit2,
        timezone,
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(([(header::CONTENT_TYPE, "image/png")], png))
}

/// GET /welcome/{mac}/*.png - Welcome/setup page for unconfigured devices
pub async fn welcome(
    State(state): State<Arc<AppState>>,
    Path((mac, _)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    state.cleanup_old_screens();

    let timezone: Tz = state.config().timezone.parse().unwrap_or(Tz::UTC);

    let inputs = state
        .take_next_screen(&mac)
        .and_then(|ns| match ns {
            NextScreen::Welcome { inputs } => Some(inputs),
            _ => None,
        })
        .unwrap_or_default();

    let png = render_png(
        &state.renderer,
        WELCOME_TEMPLATE,
        Some(inputs),
        DISPLAY_PPI,
        Depth::Bit1,
        timezone,
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(([(header::CONTENT_TYPE, "image/png")], png))
}

/// GET /screen/{mac}/*.png - Rendered screen image
pub async fn screen(
    State(state): State<Arc<AppState>>,
    Path((mac, _)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    state.cleanup_old_screens();

    let next = state.take_next_screen(&mac).ok_or(StatusCode::NOT_FOUND)?;

    let (name, inputs) = match next {
        NextScreen::Image { name, inputs } => (name, inputs),
        NextScreen::Error { .. } | NextScreen::Welcome { .. } => {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    debug!("Rendering next screen: {}", name);

    let inner_state = Arc::clone(&state);
    let renderer = state.renderer.clone();
    let timezone: Tz = state.config().timezone.parse().unwrap_or(Tz::UTC);
    let now = Instant::now();

    let result = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, (Error, Inputs)> {
        let screen = match inner_state.config().get_screen_by_name(&name) {
            Some(s) => s.clone(),
            None => return Err((Error::NoScreen(name.clone()), inputs)),
        };

        let bit_depth = if screen.option.contains(&ScreenOption::Grayscale) {
            Depth::Bit2
        } else {
            Depth::Bit1
        };

        let script = match screen.script() {
            Ok(s) => s,
            Err(e) => return Err((Error::Query(e), inputs)),
        };

        renderer
            .render(script, Some(inputs.clone()), DISPLAY_PPI, bit_depth, timezone)
            .map_err(|e| (Error::Render(e), inputs))
    })
    .await
    .expect("join failed");

    let png = match result {
        Ok(png) => {
            debug!("Rendered image in {:?}", now.elapsed());
            png
        }
        Err((Error::Render(render::Error::Typst(msg)), mut error_inputs)) => {
            warn!("Typst render error:\n{}", msg);
            error_inputs.insert("error-title".to_owned(), "Render Error".to_owned());
            error_inputs.insert("error".to_owned(), msg);
            let timezone: Tz = state.config().timezone.parse().unwrap_or(Tz::UTC);
            render_png(
                &state.renderer,
                ERROR_TEMPLATE,
                Some(error_inputs),
                DISPLAY_PPI,
                Depth::Bit2,
                timezone,
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        }
        Err((e, _)) => {
            error!("Render error: {}", e);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    Ok(([(header::CONTENT_TYPE, "image/png")], png))
}

/// GET /api/display - Main display endpoint
pub async fn display(
    State(state): State<Arc<AppState>>,
    TypedHeader(host): TypedHeader<Host>,
    headers: HeaderMap,
    device: DeviceInfo,
) -> Json<DisplayResponse> {
    debug!(
        "Device {} requesting display (battery: {:?}%)",
        device.mac_address,
        device.battery_percentage()
    );

    state.cleanup_old_screens();

    let battery_pct = calculate_battery_percentage(device.battery_voltage.unwrap_or_default());
    let mac = &device.mac_address;
    let protocol = forwarded_proto(&headers);

    let (refresh_rate, timezone, device_name) = {
        let cfg = state.config();
        let refresh_rate = cfg.get_active_refresh_rate();
        let timezone = cfg.timezone.clone();
        let device_name = match cfg.get_device_by_mac(&device.mac_address) {
            Some(d) => d.name.clone(),
            None => {
                info!("Unknown device {mac}, redirecting to welcome");
                let mut inputs = HashMap::new();
                DeviceInputs::new("Setup", battery_pct, mac, &timezone).insert_into(&mut inputs);
                inputs.insert("mac".to_owned(), mac.to_owned());
                return special_screen_response(
                    "welcome",
                    mac,
                    NextScreen::new_welcome(inputs),
                    &state,
                    host.hostname(),
                    refresh_rate,
                    protocol,
                );
            }
        };
        (refresh_rate, timezone, device_name)
    };

    let screen = match get_screen(&state, mac) {
        Ok(s) => s,
        Err(e) => {
            warn!("No screen available for {mac}: {e}");
            return special_screen_response(
                "error",
                mac,
                NextScreen::new_error(e, HashMap::new()),
                &state,
                host.hostname(),
                refresh_rate,
                protocol,
            );
        }
    };

    let (inputs, hash) = match query_inputs(
        &screen,
        DeviceInputs::new(&screen.name, battery_pct, &device_name, &timezone),
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            warn!("Input query failed for {mac}: {e}");
            let mut error_inputs = HashMap::new();
            error_inputs.insert("error-title".to_owned(), "Input Query Error".to_owned());
            error_inputs.insert("error".to_owned(), e.to_string());
            return special_screen_response(
                "error",
                mac,
                NextScreen::new_error(e, error_inputs),
                &state,
                host.hostname(),
                refresh_rate,
                protocol,
            );
        }
    };

    debug!("Next hash for {} is {}", screen.name, hash);

    image_response(
        mac,
        NextScreen::new_image(&screen.name, inputs),
        &state,
        host.hostname(),
        hash,
        refresh_rate,
        protocol,
    )
}

/// POST /api/log - Device telemetry
pub async fn log(device: DeviceInfo, Json(entry): Json<LogEntry>) -> Json<LogResponse> {
    debug!(
        "Log from {}: {:?} (battery: {:?}V)",
        device.mac_address,
        entry.log_message,
        entry
            .device_status_stamp
            .as_ref()
            .and_then(|s| s.battery_voltage)
    );

    Json(LogResponse::ok())
}
