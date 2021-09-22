use std::cell::Cell;
use std::env;
use std::sync::Arc;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

use futures::FutureExt;
use tokio::sync::mpsc::channel;
use log::{debug, error, log_enabled, info, Level};
use serenity::{
    async_trait,
    client::{Client, Context, EventHandler},
    model::channel::Message,
    model::id::{ChannelId, GuildId},
};
use songbird::{
    Event,
    EventContext,
    EventHandler as VoiceEventHandler,
    SerenityInit,
    Songbird,
    TrackEvent,
    input::restartable::Restartable,
    tracks::TrackHandle,
};
use tokio::sync::Mutex;
use unicode_prettytable::TableBuilder;
use url::Url;
use anyhow::{bail, format_err, Result};
use uuid::Uuid;

mod queue;
use queue::{Queue, Track};

#[derive(Clone)]
struct Handler {
    calls: Arc<Mutex<HashMap<ChannelId, Arc<Mutex<CallQueue>>>>>,
    track_calls: Arc<Mutex<HashMap<Uuid, ChannelId>>>,
    songbird: Arc<Songbird>,
}

struct CallQueue{
    q: Queue,
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

        let cq = self.get_call(ctx, &msg).await?;
        let mut cq = cq.lock().await;
        let t = Track::from_url(&url).await?;
        cq.q.queue.push_back(t.clone());

        if cq.now_playing.is_none() {
            debug!("nothing was playing, starting {:?}", t);
            let playing = self.start_track(&cq, &t).await?;
            cq.now_playing = Some(PlayingTrack{
                t: t,
                h: playing,
            });
        }
        // otherwise, we're done, it's queued up
        Ok(())
    }

    async fn skip(&self, ctx: &Context, msg: &Message) -> Result<()> {
        let q = self.get_call(ctx, msg).await?;
        let mut cq = q.lock().await;

        match cq.now_playing {
            None => {
                msg.channel_id
                    .say(&ctx.http, "Nothing playing to skip")
                    .await?;
                return Ok(())
            },
            Some(ref h) => {
                debug!("stopping {}", h.t.name());
                // otherwise, stop what's playing, play the next
                h.h.stop()?;
                cq.now_playing = None;
                cq.q.queue.pop_back();
                if let Some(t) = cq.q.queue.back() {
                    let playing = self.start_track(&cq, t).await?;
                    cq.now_playing = Some(PlayingTrack{
                        t: t.clone(),
                        h: playing,
                    });
                }
            }
        }
        Ok(())
    }

    async fn print_queue(&self, ctx: &Context, msg: &Message) -> Result<()> {
        let cq = self.get_call(ctx, &msg).await?;
        let cq = cq.lock().await;
        let metadata = cq.q.queue
            .iter()
            .enumerate()
            .map(|(i, t)| vec![format!("{}", i), t.name(), t.artist(), t.duration()])
            .collect::<Vec<_>>();
        println!("debug: {:?}", metadata);
        let table = TableBuilder::default()
            .rows(&metadata)
            .build()
            .map_err(|e| format_err!("error: {}", e))?;
        msg.channel_id.say(&ctx.http, format!("```\n{}\n```", table)).await?;
        Ok(())
    }

    async fn start_track(&self, cq: &CallQueue, t: &Track) -> Result<TrackHandle> {
        println!("starting track {}", t.name());
        let s: String = t.url.to_string();
        let source = Restartable::ytdl(s, true).await?;
        let mut c = cq.call.lock().await;
        let song = c.play_source(source.into());

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);

        let _ = song.add_event(
            Event::Track(TrackEvent::End),
            SongEventHandler{s: tx},
        );

        // hackery to avoid type-errors due to recursion in async fns
        let s = self.clone();
        tokio::task::spawn(Box::pin(async move {
            while let Some(t) = rx.recv().await {
                s.track_end_dyn(t).await;
            }
        }));


        Ok(song)
    }

    fn track_end_dyn(&self, t: TrackHandle) -> impl futures::future::Future<Output = ()> + Send {
        let await_end = self.clone();
        async move {
            await_end.track_end(&t).await
        }
    }

    // get_call gets a call, and joins it if we're not in it
    async fn get_call(&self, ctx: &Context, msg: &Message) -> Result<Arc<Mutex<CallQueue>>> {
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

    async fn get_call_by_channel_id(&self, gid: GuildId, cid: ChannelId) -> Result<Arc<Mutex<CallQueue>>> {
        let mut cl = self.calls.lock().await;

        let v = match cl.entry(cid) {
            Entry::Occupied(c) => {
                return Ok(c.get().clone());
            }
            Entry::Vacant(v) => v,
        };

        let (h, success) = self.songbird.join(gid, cid).await;
        success?;
        h.lock().await.deafen(true).await?;
        let cq = Arc::new(Mutex::new(CallQueue{
            q: Default::default(),
            now_playing: None,
            call: h,
            guild_id: gid,
            channel_id: cid,
        }));
        // cache for next time
        v.insert(cq.clone());
        Ok(cq)
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
                println!("track ended for a channel we don't have a queue for {:?}", channel_id);
            },
            Some(cq) => {
                let mut cq = cq.lock().await;
                match cq.now_playing {
                    None => {
                        println!("track ended for a channel we don't have a now_playing for {:?}", channel_id);
                        return;
                    },
                    Some(ref t) => {
                        if t.h.uuid() != u {
                            println!("track ended but was wrong uuid, weird {:?}, {:?}", h, t.h);
                            return
                        }
                        cq.now_playing = None;
                        cq.q.queue.pop_back();
                        if let Some(t) = cq.q.queue.back() {
                            let h = match self.start_track(&cq, &t).await {
                                Ok(h) => h,
                                Err(e) => {
                                    println!("error playing {:?}", e);
                                    // TODO: skip
                                    return;
                                },
                            };
                            cq.now_playing = Some(PlayingTrack{
                                h: h,
                                t: t.clone(),
                            });
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
        match ctx {
            EventContext::Track([ts]) => {
                match ts.0.playing {
                    songbird::tracks::PlayMode::End => {
                        println!("Song ending: {:?}", ts.1);
                        self.s.send(ts.1.clone()).await.unwrap();
                        None
                    }
                    _ => return None,
                }
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
            return
        }
        let s = &s[1..];
        debug!("command: {}", s);
        match s {
            s if s.starts_with("play ") => {
                debug!("play {}", s[5..].trim());
                match self.play(&ctx, &msg, s[5..].trim()).await {
                    Ok(_) => {
                    }
                    Err(e) => {
                        println!("err: {:?}", e);
                    }
                }
            }
            s if s == "skip" => {
                debug!("skip");
                match self.skip(&ctx, &msg).await {
                    Ok(_) => {
                    }
                    Err(e) => {
                        println!("err: {:?}", e);
                    }
                }
            }
            s if s == "queue" => {
                debug!("queue");
                match self.print_queue(&ctx, &msg).await {
                    Ok(_) => {
                    }
                    Err(e) => {
                        println!("err: {:?}", e);
                    }
                }
            }
            _ => {},
        }

    }

    async fn ready(&self, _: Context, ready: serenity::model::gateway::Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let queue = Queue::default();

    let songbird = Songbird::serenity();

    let handler = Handler{
        songbird: songbird.clone(),
        calls: Arc::new(Mutex::new(HashMap::new())),
        track_calls: Arc::new(Mutex::new(HashMap::new())),
    };


    // Login with a bot token from the environment
    let token = env::var("DISCORD_TOKEN").expect("token");
    let client_builder = Client::builder(token)
        .event_handler(handler)
        .register_songbird()
        .type_map_insert::<queue::QueueKey>(Arc::new(queue));
    let client_builder = songbird::serenity::register_with(client_builder, songbird);
    let mut client = client_builder
        .await
        .expect("Error creating client");

    // start listening for events by starting a single shard
    if let Err(why) = client.start().await {
        println!("An error occurred while running the client: {:?}", why);
    }
}

#[async_trait]
impl VoiceEventHandler for Queue {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(track_list) = ctx {
            println!("Track ended: {:?}", track_list);
        }
        None
    }
}
