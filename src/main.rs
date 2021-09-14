use std::cell::Cell;
use std::env;
use std::sync::Arc;

use serenity::{
    async_trait,
    client::{Client, Context, EventHandler},
    framework::standard::{
        Args,
        CommandResult, StandardFramework,
        macros::{command, group},
    },
    model::channel::Message,
    model::id::GuildId,
};
use songbird::{
    Event,
    EventContext,
    EventHandler as VoiceEventHandler,
    SerenityInit,
    Songbird,
    TrackEvent,
    input::restartable::Restartable,
};
use tokio::sync::Mutex;
use unicode_prettytable::TableBuilder;
use url::Url;

mod queue;
use queue::Queue;

#[group]
#[commands(play, join, queue)]
struct General;

struct Handler;

#[async_trait]
impl EventHandler for Handler {}

#[tokio::main]
async fn main() {
    let framework = StandardFramework::new()
        .configure(|c| c.prefix(r#"""#))
        .group(&GENERAL_GROUP);

    let queue = Queue::default();

    // Login with a bot token from the environment
    let token = env::var("DISCORD_TOKEN").expect("token");
    let mut client = Client::builder(token)
        .event_handler(Handler)
        .framework(framework)
        .register_songbird()
        .type_map_insert::<queue::QueueKey>(Arc::new(queue))
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

#[command]
async fn join(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).await.unwrap();
    let channel_id = guild
        .voice_states
        .get(&msg.author.id)
        .and_then(|voice_state| voice_state.channel_id);

    let connect_to = match channel_id {
        Some(channel) => channel,
        None => {
            msg.reply(ctx, "You're not in a voice channel").await?;
            return Ok(());
        }
    };

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let (h, success) = manager.join(guild.id, connect_to).await;
    success?;
    let mut h = h.lock().await;
    h.deafen(true).await?;

    Ok(())
}

#[command]
async fn queue(ctx: &Context, msg: &Message) -> CommandResult {
    let data = ctx.data.read().await;
    let q_arc = data.get::<queue::QueueKey>().unwrap();
    let q = q_arc.queue.lock().await;
    if q.len() == 0 {
        msg.channel_id.say(&ctx.http, format!("No songs")).await?;
        return Ok(());
    }

    let metadata = q
        .iter()
        .enumerate()
        .map(|(i, t)| vec![format!("{}", i), t.name(), t.artist(), t.duration()])
        .collect::<Vec<_>>();
    let table = TableBuilder::default()
        .rows(&metadata)
        .build()?;
    msg.channel_id.say(&ctx.http, format!("```\n{}\n```", table)).await?;
    Ok(())
}

#[command]
async fn play(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    join(ctx, msg, Args::new("", &vec![])).await?;

    let url = match args.single::<String>() {
        Ok(url) => url,
        Err(_) => {
            msg.channel_id
                .say(&ctx.http, "Must provide a URL to a video or audio")
                .await?;

            return Ok(());
        }
    };

    let url = match Url::parse(&url) {
        Err(_) => {
            msg.reply(ctx, format!("unable to parse URL {:?}", url))
                .await?;
            return Ok(());
        }
        Ok(u) => u,
    };

    let guild = msg.guild(&ctx.cache).await.unwrap();
    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let handler_lock = match manager.get(guild.id) {
        Some(h) => h,
        None => {
            msg.channel_id
                .say(&ctx.http, "Not in a voice channel to play in")
                .await?;
            return Ok(());
        }
    };
    let mut handler = handler_lock.lock().await;

    let source = match songbird::ytdl(&url).await {
        Ok(source) => source,
        Err(e) => {
            println!("Err starting source: {:?}", e);
            msg.channel_id
                .say(&ctx.http, "Error sourcing ffmpeg")
                .await?;
            return Ok(());
        }
    };
    let metadata = source.metadata;
    let source2 = Restartable::ytdl(url, true).await?;

    let data = ctx.data.read().await;
    let q = data.get::<queue::QueueKey>().unwrap();
    let mut track = queue::Track {
        source: Arc::new(Mutex::new(Cell::new(None))),
        metadata,
        http: ctx.http.clone(),
    };
    let name = track.name();

    if q.len().await == 0 {
        msg.channel_id
            .say(&ctx.http, format!("Playing song: {}", name))
            .await?;
        let song = handler.play_source(source2.into());
        q.enqueue(track).await;

        song.add_event(
            Event::Track(TrackEvent::End),
            SongEndHandler {
                q: q.clone(),
                guild_id: guild.id.clone(),
                manager: manager,
            },
        )?;
    } else {
        track.source = Arc::new(Mutex::new(Cell::new(Some(source2))));
        q.enqueue(track).await;
        msg.channel_id
            .say(&ctx.http, format!("Enqueued song; {} to go before it", q.len().await - 1))
            .await?;
    }
    Ok(())
}

struct SongEndHandler {
    q: Arc<Queue>,
    manager: Arc<Songbird>,
    guild_id: GuildId,
}

#[async_trait]
impl VoiceEventHandler for SongEndHandler {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        let q = self.q.clone();
        let played = q.dequeue().await.unwrap();

        println!("Song finished: {}", played.name());
        if q.len().await == 0 {
            println!("Done with songs");
        } else {
            let next = q.peek().await.unwrap();
            let source = next.source.lock().await.replace(None);
            let manager = self.manager.clone();
            if source.is_none() {
                println!("None source");
                return None;
            }

            let handler_lock = match manager.get(self.guild_id) {
                Some(h) => h,
                None => {
                    println!("Not in a voice channel to play in");
                    return None;
                }
            };
            let mut handler = handler_lock.lock().await;
            let song = handler.play_source(source.unwrap().into());

            let _ = song.add_event(
                Event::Track(TrackEvent::End),
                SongEndHandler {
                    q: self.q.clone(),
                    guild_id: self.guild_id,
                    manager: self.manager.clone(),
                },
            );
        }
        None
    }
}
