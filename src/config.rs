use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Datelike, NaiveTime, TimeZone, Timelike, Utc, Weekday};
use futures::stream::{self, StreamExt};
use tokio::{fs, process::Command};

use reqwest::IntoUrl;
use tracing::{debug, warn};
use trmnl::DaySelector;

pub type Inputs = HashMap<String, String>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Error reading config: {0}")]
    Io(#[from] std::io::Error),
    #[error("Error deserializing config: {0}")]
    Deserialize(#[from] serde_yaml::Error),
    #[error("Error fetching URL: {0}")]
    FetchUrl(#[from] reqwest::Error),
    #[error("HTTP {status} from {url}")]
    UrlStatusCode { status: u16, url: String },
    #[error("shell command `{command}` failed (exit {exit_code:?}):\n{stderr}")]
    CommandFailed {
        command: String,
        exit_code: Option<i32>,
        stderr: String,
    },
    #[error("invalid UTF-8 in command output: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Device {
    pub mac_address: String,
    pub name: String,
}

#[derive(Copy, Clone, Debug, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistOption {}

#[derive(Copy, Clone, Debug, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ScreenOption {
    Grayscale,
}

#[derive(Copy, Clone, Debug, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum InputOption {
    NoHash,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Playlist {
    pub name: String,
    pub screen: Vec<String>,
    #[serde(default)]
    pub option: Vec<PlaylistOption>,
}

impl Playlist {
    // Get the next screen in a particular playlist
    pub fn get_next_screen<'a>(&self, config: &'a Config, counter: usize) -> Option<&'a Screen> {
        if self.screen.len() == 0 {
            warn!("Playlist \"{}\" has no screens", self.name);
            return None;
        }

        let next_screen = &self.screen[counter % self.screen.len()];

        if let Some(screen) = config.get_screen_by_name(&next_screen) {
            debug!("Next screen is \"{}\"", screen.name);
            return Some(screen);
        }

        warn!("Screen with name \"{}\" does not exist", next_screen);
        None
    }

    pub fn has_option(&self, option: PlaylistOption) -> bool {
        self.option.contains(&option)
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    File(PathBuf),
    Url(String),
    Shell(String),
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Input {
    pub name: String,
    #[serde(flatten)]
    pub typ: InputType,
    #[serde(default)]
    pub option: Vec<InputOption>,
}

impl Input {
    pub fn has_option(&self, option: InputOption) -> bool {
        self.option.contains(&option)
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenScript {
    File(PathBuf),
    Inline(String),
    #[serde(skip)]
    Cached(String),
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Screen {
    pub name: String,
    #[serde(flatten)]
    pub script: ScreenScript,
    #[serde(default)]
    pub input: Vec<Input>,
    #[serde(default)]
    pub option: Vec<ScreenOption>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Schedule {
    pub playlist: Option<String>,
    pub days: trmnl::DaySelector,
    pub start: String,
    pub end: String,
    pub refresh_rate: Option<u32>,
}

impl Schedule {
    // Whether this schedule matches the given weekday and time
    fn matches(&self, weekday: Weekday, time: NaiveTime) -> bool {
        if !self.day_matches(weekday) {
            return false;
        }

        let start = parse_time(&self.start);
        let end = parse_time(&self.end);

        match (start, end) {
            (Some(s), Some(e)) => {
                if s <= e {
                    // during the day
                    time >= s && time < e
                } else {
                    // over midnight
                    time >= s || time < e
                }
            }
            _ => false,
        }
    }

    // Whether this schedule matches the given weekday
    fn day_matches(&self, weekday: Weekday) -> bool {
        match &self.days {
            DaySelector::Named(name) => match name.to_lowercase().as_str() {
                "all" => true,
                "weekdays" => matches!(
                    weekday,
                    Weekday::Mon | Weekday::Tue | Weekday::Wed | Weekday::Thu | Weekday::Fri
                ),
                "weekends" => matches!(weekday, Weekday::Sat | Weekday::Sun),
                _ => weekday_from_str(name) == Some(weekday),
            },
            DaySelector::List(days) => days.iter().any(|d| weekday_from_str(d) == Some(weekday)),
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Config {
    pub device: Vec<Device>,
    pub playlist: Vec<Playlist>,
    pub screen: Vec<Screen>,
    pub schedule: Vec<Schedule>,
    pub default_playlist: String,
    pub default_refresh_rate: u32,
    pub timezone: String,
}

impl Config {
    // Load config from a file
    pub fn load(path: impl AsRef<Path>) -> Result<(Self, PathBuf), Error> {
        let path = path.as_ref();
        let config = std::fs::read_to_string(path)?;
        let config_dir = path
            .parent()
            .expect("Parent directory of config file is not a directory");

        let mut config: Self = serde_yaml::from_str(&config)?;

        let preamble = include_str!("templates/preamble.typ");

        for screen in &mut config.screen {
            match &screen.script {
                ScreenScript::File(p) => {
                    let script = std::fs::read_to_string(config_dir.join(&*p))?;
                    screen.script = ScreenScript::Cached(script);
                }
                ScreenScript::Inline(s) => {
                    screen.script = ScreenScript::Cached(format!("{preamble}{s}"));
                }
                ScreenScript::Cached(_) => {}
            }
        }
        Ok((config, config_dir.to_path_buf()))
    }

    // Get a device by its MAC address
    pub fn get_device_by_mac(&self, mac: &str) -> Option<&Device> {
        let device = self
            .device
            .iter()
            .find(|d| d.mac_address.eq_ignore_ascii_case(mac));
        if device.is_none() {
            warn!("No device with MAC address {} found", mac);
        }
        device
    }

    // Get the playlist for the currently active schedule
    pub fn get_active_playlist(&self) -> Option<&Playlist> {
        if let Some(schedule) = self.get_active_schedule()
            && let Some(playlist_name) = &schedule.playlist
            && let Some(playlist) = self.get_playlist_by_name(playlist_name)
        {
            return Some(playlist);
        }

        debug!("No playlist in current schedule, using default playlist");

        if let Some(playlist) = self.get_playlist_by_name(&self.default_playlist) {
            return Some(playlist);
        }

        warn!(
            "Found no default playlist with name \"{}\"",
            self.default_playlist
        );

        None
    }

    // Get a playlist by name
    pub fn get_playlist_by_name(&self, playlist: &str) -> Option<&Playlist> {
        let found = self.playlist.iter().find(|p| p.name == playlist);
        if found.is_none() {
            warn!("No playlist with name \"{}\" found", playlist);
        }
        found
    }

    // Get a screen by name
    pub fn get_screen_by_name(&self, screen: &str) -> Option<&Screen> {
        let found = self.screen.iter().find(|s| s.name == screen);
        if found.is_none() {
            warn!("No screen with name \"{}\" found", screen);
        }
        found
    }

    // Get the currently active refresh_rate
    pub fn get_active_refresh_rate(&self) -> u32 {
        if let Some(schedule) = self.get_active_schedule()
            && let Some(refresh_rate) = schedule.refresh_rate
        {
            return refresh_rate;
        }

        debug!(
            "No refresh rate configured in active schedule. Falling back to default_refresh_rate = {}",
            self.default_refresh_rate
        );
        self.default_refresh_rate
    }

    // Get the currently active schedule
    pub fn get_active_schedule(&self) -> Option<&Schedule> {
        let tz = self.timezone.parse().unwrap_or_else(|_| {
            warn!("No timezone \"{}\", defaulting to UTC", self.timezone);
            chrono_tz::UTC
        });

        let now = Utc::now().with_timezone(&tz);

        self.get_schedule_for_time(now)
    }

    // Get the schedule for a specific time
    pub fn get_schedule_for_time<T: TimeZone>(&self, dt: DateTime<T>) -> Option<&Schedule> {
        let weekday = dt.weekday();
        let time = NaiveTime::from_hms_opt(dt.hour(), dt.minute(), 0).unwrap_or_default();
        let found = self
            .schedule
            .iter()
            .find(|rule| rule.matches(weekday, time));
        if found.is_none() {
            warn!("No schedule found for time {:?}", dt);
        }
        found
    }
}

impl Screen {
    pub fn script(&self) -> Result<String, Error> {
        Ok(match &self.script {
            ScreenScript::File(p) => std::fs::read_to_string(p)?,
            ScreenScript::Inline(s) | ScreenScript::Cached(s) => s.clone(),
        })
    }

    pub fn has_option(&self, option: ScreenOption) -> bool {
        self.option.contains(&option)
    }

    pub async fn query_inputs(&self) -> Result<(Inputs, u64), Error> {
        let query_results: Vec<Result<_, Error>> = stream::iter(&self.input)
            .then(async |i| {
                debug!("Querying input {:?} ({:?})", i.name, i.typ);
                let result = match &i.typ {
                    InputType::File(p) => fs::read_to_string(p).await.map_err(Error::from),
                    InputType::Shell(command) => run_shell_command(command).await,
                    InputType::Url(url) => query_url(url).await,
                };
                match &result {
                    Err(e) => warn!("Input {:?} failed: {}", i.name, e),
                    Ok(_) => debug!("Input {:?} fetched ok", i.name),
                }
                result.map(|value| {
                    (
                        i.name.clone(),
                        value,
                        i.option.contains(&InputOption::NoHash),
                    )
                })
            })
            .collect()
            .await;

        // calculate input-hash (ignore inputs marked with no_hash)
        let mut hasher = DefaultHasher::new();

        let result: Result<Inputs, Error> = query_results
            .into_iter()
            .map(|r| {
                r.map(|(name, value, no_hash)| {
                    if !no_hash {
                        name.hash(&mut hasher);
                        value.hash(&mut hasher);
                    }
                    (name, value)
                })
            })
            .collect();

        result.map(|r| (r, hasher.finish()))
    }
}

static HTTP_CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("tymnl/1.0")
        .build()
        .expect("Failed to build HTTP client")
});

async fn query_url(url: impl IntoUrl) -> Result<String, Error> {
    let url = url.into_url()?;
    debug!("Fetching URL: {}", url);

    let response = HTTP_CLIENT.get(url).send().await?;
    let status = response.status();

    if !status.is_success() {
        let url = response.url().to_string();
        warn!("HTTP {} from {}", status, url);
        return Err(Error::UrlStatusCode {
            status: status.as_u16(),
            url,
        });
    }

    Ok(response.text().await?)
}

async fn run_shell_command(command: &str) -> Result<String, Error> {
    debug!("Running shell command: {:?}", command);
    let output = Command::new("bash").args(["-c", command]).output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        warn!(
            "Shell command {:?} failed (exit {:?}):\n{}",
            command,
            output.status.code(),
            stderr
        );
        return Err(Error::CommandFailed {
            command: command.to_owned(),
            exit_code: output.status.code(),
            stderr,
        });
    }

    if !output.stderr.is_empty() {
        debug!(
            "Shell command {:?} wrote to stderr: {}",
            command,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8(output.stdout)?)
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    let (h, m) = s.split_once(':')?;
    NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0)
}

fn weekday_from_str(s: &str) -> Option<Weekday> {
    Some(match s.to_lowercase().as_str() {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tuesday" => Weekday::Tue,
        "wed" | "wednesday" => Weekday::Wed,
        "thu" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        _ => return None,
    })
}
