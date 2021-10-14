use std::collections::hash_map::Entry;
use std::collections::{VecDeque, HashMap};
use std::env;
use std::sync::Arc;

use anyhow::{bail, format_err, Result};
use log::debug;
use serenity::{
    async_trait,
    client::{Client, Context, EventHandler},
    model::channel::Message,
    model::id::{ChannelId, GuildId},
};
use songbird::{
    input::restartable::Restartable, tracks::TrackHandle, Event, EventContext,
    EventHandler as VoiceEventHandler, SerenityInit, Songbird, TrackEvent,
};
use tokio::sync::Mutex;
use unicode_prettytable::TableBuilder;
use url::Url;
use uuid::Uuid;

#[derive(Clone)]
struct Handler {
    calls: Arc<Mutex<HashMap<GuildId, Arc<Mutex<CallQueue>>>>>,
    track_calls: Arc<Mutex<HashMap<Uuid, GuildId>>>,
    songbird: Arc<Songbird>,
}

struct CallQueue {
    q: VecDeque<Track>,
    now_playing: Option<PlayingTrack>,
    call: Arc<Mutex<songbird::Call>>,
    guild_id: GuildId,
    channel_id: ChannelId,
}

struct PlayingTrack {
    t: Track,
    h: TrackHandle,
}

impl Handler {
    async fn play(&self, ctx: &Context, msg: &Message, url: &str) -> Result<()> {
        // Parse
        let url = match Url::parse(&url) {
            Err(_) => {
                msg.reply(ctx, format!("unable to parse URL {:?}", url))
                    .await?;
                return Ok(());
            }
            Ok(u) => u,
        };

        let cq = self.get_or_join_call(ctx, &msg).await?;
        let mut cq = cq.lock().await;
        let t = Track::from_url(&url).await?;
        cq.q.push_back(t.clone());

        if cq.now_playing.is_none() {
            debug!("nothing was playing, starting {:?}", t);
            let playing = self.start_track(&cq, &t).await?;
            cq.now_playing = Some(PlayingTrack { t: t, h: playing });
        }
        // otherwise, we're done, it's queued up
        Ok(())
    }

    async fn skip(&self, ctx: &Context, msg: &Message) -> Result<()> {
        let q = self.get_or_join_call(ctx, msg).await?;
        let mut cq = q.lock().await;

        match cq.now_playing {
            None => {
                msg.channel_id
                    .say(&ctx.http, "Nothing playing to skip")
                    .await?;
                return Ok(());
            }
            Some(ref h) => {
                debug!("stopping {}", h.t.name());
                // otherwise, stop what's playing.
                h.h.stop()?;

                cq.now_playing = None;
                cq.q.pop_front();
                if let Some(t) = cq.q.front() {
                    let playing = self.start_track(&cq, t).await?;
                    cq.now_playing = Some(PlayingTrack {
                        t: t.clone(),
                        h: playing,
                    });
                }
            }
        }
        Ok(())
    }

    async fn print_queue(&self, ctx: &Context, msg: &Message) -> Result<()> {
        let cq = match self.get_call_by_guild_id(msg.guild(&ctx.cache).await.unwrap().id).await? {
            None => {
                bail!("not in any channel");
            },
            Some(cq) => cq,
        };
        let cq = cq.lock().await;
        let metadata =
            cq.q
                .iter()
                .enumerate()
                .map(|(i, t)| vec![format!("{}", i), t.name(), t.artist(), t.duration()])
                .collect::<Vec<_>>();
        if metadata.len() == 0 {
            bail!("No songs playing");
        }
        let table = TableBuilder::default()
            .rows(&metadata)
            .build()
            .map_err(|e| format_err!("{}", e))?;
        msg.channel_id
            .say(&ctx.http, format!("```\n{}\n```", table))
            .await?;
        Ok(())
    }

    async fn disconnect(&self, ctx: &Context, msg: &Message) -> Result<()> {
        let guild = msg.guild(&ctx.cache).await.unwrap();
        let mut cl = self.calls.lock().await;
        let c = match cl.remove(&guild.id) {
            Some(c) => c,
            None => {
                bail!("not in any voice channel");
            },
        };

        {
            let cq = c.lock().await;
            if let Some(ref playing) = cq.now_playing {
                let _ = playing.h.stop();
            }
            cq.call.lock().await.leave().await?;
        }

        Ok(())
    }

    async fn playing(&self, ctx: &Context, msg: &Message) -> Result<()> {
        let cq = match self.get_call_by_guild_id(msg.guild(&ctx.cache).await.unwrap().id).await? {
            None => {
                bail!("not in any channel");
            },
            Some(cq) => cq,
        };
        let cq = cq.lock().await;

        match cq.now_playing {
            None => {
                bail!("no song is playing");
            }
            Some(ref t) => {
                let info = t.h.get_info().await?;
                match t.t.metadata.duration {
                    None => {
                        bail!("no duration metadata");
                    }
                    Some(d) => {
                        msg.channel_id
                            .say(&ctx.http, format!("Playing: {}\nLeft: {}", t.t.name(), format_duration(&(d - info.position))))
                            .await?;
                    }
                }
            },
        }

        Ok(())

    }

    async fn start_track(&self, cq: &CallQueue, t: &Track) -> Result<TrackHandle> {
        println!("starting track {}", t.name());
        let s: String = t.url.to_string();
        let source = Restartable::ytdl(s, true).await?;
        let mut c = cq.call.lock().await;
        let song = c.play_source(source.into());
        self.track_calls.lock().await.insert(song.uuid(), cq.guild_id);

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);

        let _ = song.add_event(Event::Track(TrackEvent::End), SongEventHandler { s: tx });

        // hackery to avoid type-errors due to recursion in async fns
        let s = self.clone();
        tokio::task::spawn(Box::pin(async move {
            while let Some(t) = rx.recv().await {
                debug!("track end event");
                s.track_end_dyn(t).await;
            }
        }));

        Ok(song)
    }

    fn track_end_dyn(&self, t: TrackHandle) -> impl futures::future::Future<Output = ()> + Send {
        let await_end = self.clone();
        async move { await_end.track_end(&t).await }
    }

    // get_call gets a call, and joins it if we're not in it
    async fn get_or_join_call(&self, ctx: &Context, msg: &Message) -> Result<Arc<Mutex<CallQueue>>> {
        let guild = msg.guild(&ctx.cache).await.unwrap();
        let channel_id = guild
            .voice_states
            .get(&msg.author.id)
            .and_then(|voice_state| voice_state.channel_id);
        let channel_id = match channel_id {
            Some(channel) => channel,
            None => {
                bail!("not in a voice channel");
            }
        };

        self.get_call_by_channel_id(guild.id, channel_id).await
    }

    async fn get_call_by_channel_id(
        &self,
        gid: GuildId,
        cid: ChannelId,
    ) -> Result<Arc<Mutex<CallQueue>>> {
        let mut cl = self.calls.lock().await;

        let v = match cl.entry(gid) {
            Entry::Occupied(c) => {
                let cq = c.get().clone();
                if cq.lock().await.channel_id != cid {
                    return Err(format_err!("already in another channel: {}", cid));
                }
                return Ok(c.get().clone());
            }
            Entry::Vacant(v) => v,
        };

        let (h, success) = self.songbird.join(gid, cid).await;
        success?;
        h.lock().await.deafen(true).await?;
        let cq = Arc::new(Mutex::new(CallQueue {
            q: Default::default(),
            now_playing: None,
            call: h,
            channel_id: cid,
            guild_id: gid,
        }));
        // cache for next time
        v.insert(cq.clone());
        Ok(cq)
    }

    async fn get_call_by_guild_id(
        &self,
        gid: GuildId,
    ) -> Result<Option<Arc<Mutex<CallQueue>>>> {
        let mut cl = self.calls.lock().await;
        match cl.entry(gid) {
            Entry::Occupied(c) => {
                Ok(Some(c.get().clone()))
            }
            Entry::Vacant(_) => Ok(None)
        }
    }

    async fn track_end(&self, h: &TrackHandle) {
        let u = h.uuid();
        let channel_id = match self.track_calls.lock().await.remove(&u) {
            None => {
                println!("track ended but we weren't playing it: {:?}", u);
                return;
            }
            Some(cid) => cid,
        };

        let mut hm = self.calls.lock().await;
        match hm.get_mut(&channel_id) {
            None => {
                println!(
                    "track ended for a channel we don't have a queue for {:?}",
                    channel_id
                );
            }
            Some(cq) => {
                let mut cq = cq.lock().await;
                match cq.now_playing {
                    None => {
                        println!(
                            "track ended for a channel we don't have a now_playing for {:?}",
                            channel_id
                        );
                        return;
                    }
                    Some(ref t) => {
                        if t.h.uuid() != u {
                            println!("track ended but was wrong uuid, weird {:?}, {:?}", h, t.h);
                            return;
                        }
                        cq.now_playing = None;
                        cq.q.pop_front();
                        if let Some(t) = cq.q.front() {
                            let h = match self.start_track(&cq, &t).await {
                                Ok(h) => h,
                                Err(e) => {
                                    println!("error playing {:?}", e);
                                    // TODO: skip
                                    return;
                                }
                            };
                            cq.now_playing = Some(PlayingTrack { h: h, t: t.clone() });
                        }
                    }
                }
            }
        }
    }
}

struct SongEventHandler {
    s: tokio::sync::mpsc::Sender<TrackHandle>,
}

#[async_trait]
impl VoiceEventHandler for SongEventHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        debug!("voice event handler: {:?}", ctx);
        match ctx {
            EventContext::Track([ts]) => match ts.0.playing {
                songbird::tracks::PlayMode::End => {
                    println!("Song ending: {:?}", ts.1);
                    self.s.send(ts.1.clone()).await.unwrap();
                    None
                }
                _ => return None,
            },
            _ => return None,
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        let s = &msg.content;
        debug!("msg: {}", s);
        if !s.starts_with('"') {
            return;
        }
        let s = &s[1..];
        debug!("command: {}", s);
        let res = match s {
            s if s.starts_with("play ") => {
                debug!("play {}", s[5..].trim());
                self.play(&ctx, &msg, s[5..].trim()).await
            }
            s if s == "skip" => {
                self.skip(&ctx, &msg).await
            }
            s if s == "queue" => {
                self.print_queue(&ctx, &msg).await
            }
            s if s == "disconnect" => {
                self.disconnect(&ctx, &msg).await
            }
            s if s == "playing" => {
                self.playing(&ctx, &msg).await
            }
            s if s == "help" => {
                Err(format_err!("```Help\n\tplay <url> - Play a url that youtube-dl supports\n\tskip - Skip\n\tqueue - Show the queueueue\n\tdisconnect\n\tplaying - Info about the playing song```"))
            }
            _ => {
                Ok(())
            }
        };
        match res {
            Err(e) => {
                let _ = msg.channel_id
                    .say(&ctx.http, format!("err: {}", e))
                    .await;
            }
            Ok(_) => {},
        }
    }

    async fn ready(&self, _: Context, ready: serenity::model::gateway::Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let songbird = Songbird::serenity();

    let handler = Handler {
        songbird: songbird.clone(),
        calls: Arc::new(Mutex::new(HashMap::new())),
        track_calls: Arc::new(Mutex::new(HashMap::new())),
    };

    // Login with a bot token from the environment
    let token = env::var("DISCORD_TOKEN").expect("token");
    let client_builder = Client::builder(token)
        .event_handler(handler)
        .register_songbird();
    let client_builder = songbird::serenity::register_with(client_builder, songbird);
    let mut client = client_builder.await.expect("Error creating client");

    // start listening for events by starting a single shard
    if let Err(why) = client.start().await {
        println!("An error occurred while running the client: {:?}", why);
    }
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
        match &self.metadata.duration {
            None => return "<no duration>".to_string(),
            Some(s) => format_duration(s),
        }
    }
}

fn format_duration(d: &std::time::Duration) -> String {
    let mut s = d.as_secs();
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
