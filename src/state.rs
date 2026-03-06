use std::{
    collections::HashMap,
    sync::{Mutex, RwLock},
    time::{Duration, Instant},
};

use tymnl::{
    config::{self, Config, Inputs},
    render::{self, Renderer},
};

const MAX_STATE_AGE: Duration = Duration::from_secs(3600);

#[derive(Debug)]
pub enum NextScreen {
    Image { name: String, inputs: Inputs },
    Error { error: Error, inputs: Inputs },
    Welcome { inputs: Inputs },
}

impl NextScreen {
    pub fn new_image(name: &str, inputs: Inputs) -> Self {
        Self::Image {
            name: name.to_owned(),
            inputs,
        }
    }

    pub fn new_error(error: Error, inputs: Inputs) -> Self {
        Self::Error { error, inputs }
    }

    pub fn new_welcome(inputs: Inputs) -> Self {
        Self::Welcome { inputs }
    }
}

#[derive(Debug)]
pub struct AppState {
    pub config: RwLock<Config>,
    pub renderer: Renderer,
    pub playlist_indices: Mutex<HashMap<String, usize>>,
    pub next_screens: Mutex<HashMap<String, (Instant, NextScreen)>>,
}

impl AppState {
    pub fn config(&self) -> std::sync::RwLockReadGuard<'_, Config> {
        self.config.read().expect("config lock poisoned")
    }

    pub fn next_playlist_index(&self, mac: &str) -> usize {
        let mut indices = self
            .playlist_indices
            .lock()
            .expect("Can't lock AppState.playlist_indices mutex");
        let index = indices.entry(mac.to_owned()).or_insert(0);
        let current = *index;
        *index = index.wrapping_add(1);
        current
    }

    pub fn take_next_screen(&self, mac: &str) -> Option<NextScreen> {
        self.next_screens
            .lock()
            .expect("Can't lock AppState.next_screens mutex")
            .remove(mac)
            .map(|(_, next)| next)
    }

    pub fn put_next_screen(&self, mac: &str, next: NextScreen) {
        self.next_screens
            .lock()
            .expect("Can't lock AppState.next_screens mutex")
            .insert(mac.to_owned(), (Instant::now(), next));
    }

    pub fn cleanup_old_screens(&self) {
        self.next_screens
            .lock()
            .expect("Can't lock AppState.next_screens mutex")
            .retain(|_, (inserted_at, _)| inserted_at.elapsed() < MAX_STATE_AGE);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no active playlist defined")]
    NoActivePlaylist,
    #[error("no screen defined for playlist {0}")]
    NoScreen(String),
    #[error("querying input failed: {0}")]
    Query(#[from] config::Error),
    #[error("rendering failed")]
    Render(#[from] render::Error),
    #[error("unknown error")]
    Unknown,
}
