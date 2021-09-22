use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::Result;
use serenity::http::Http;
use songbird::input::restartable::Restartable;
use tokio::sync::Mutex;
use url::Url;

// Queue is added to the storage ctx of serenity to manage the currently playing tracks.
// songbird has its own trackqueue thing, but I felt like implementing my own, and it's not much
// code.
#[derive(Default)]
pub struct Queue {
    pub queue: VecDeque<Track>,
}

pub struct QueueKey;

impl serenity::prelude::TypeMapKey for QueueKey {
    type Value = Arc<Queue>;
}

#[derive(Clone, Debug)]
pub struct Track {
    pub metadata: Box<songbird::input::Metadata>,
    pub url: Url,
}

impl Track {
    pub async fn from_url(url: &Url) -> Result<Self> {
        let source = songbird::ytdl(&url).await?;
        let metadata = source.metadata;
        Ok(Track {
            metadata: metadata,
            url: url.clone(),
        })
    }

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
