use std::collections::VecDeque;
use std::cell::Cell;
use std::sync::Arc;

use serenity::http::Http;
use songbird::input::restartable::Restartable;
use tokio::sync::Mutex;

// Queue is added to the storage ctx of serenity to manage the currently playing tracks.
// songbird has its own trackqueue thing, but I felt like implementing my own, and it's not much
// code.
#[derive(Default)]
pub struct Queue {
    pub queue: Mutex<VecDeque<Track>>,
}

pub struct QueueKey;

impl serenity::prelude::TypeMapKey for QueueKey {
    type Value = Arc<Queue>;
}

#[derive(Clone)]
pub struct Track {
    pub source: Arc<Mutex<Cell<Option<Restartable>>>>,
    pub metadata: Box<songbird::input::Metadata>,
    pub http: Arc<Http>,
}

impl Track {
    pub fn name(&self) -> String {
        match &self.metadata.title {
            None => "<no title>".to_string(),
            Some(s) => s.to_string(),
        }
    }

    pub fn artist(&self) -> String {
        match &self.metadata.artist {
            None => "<no artist>".to_string(),
            Some(s) => s.to_string(),
        }
    }

    pub fn duration(&self) -> String {
        let mut s = match &self.metadata.duration {
            None => return "<no duration>".to_string(),
            Some(s) => s.as_secs(),
        };
        let (mut h, mut m) = (0, 0);
        while s > 60 * 60 {
            h += 1;
            s -= 60 * 60;
        }
        while s > 60 {
            m += 1;
            s -= 60;
        }
        format!("{}h{}m{}s", h, m, s)
    }
}
