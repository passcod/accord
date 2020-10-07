#![doc(html_favicon_url = "https://raw.githubusercontent.com/passcod/accord/main/res/logo.png")]
#![doc(html_logo_url = "https://raw.githubusercontent.com/passcod/accord/main/res/logo.png")]

use accord::{handle_event, raccord};
use async_channel::{unbounded, Receiver, Sender};
use async_std::{
    prelude::{FutureExt, StreamExt},
    task::spawn,
};
use isahc::ResponseExt;
use std::{env, error::Error, fmt::Debug, str::FromStr, sync::Arc};
use tide::Server;
use tide_tracing::TraceMiddleware;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use twilight_cache_inmemory::{EventType, InMemoryCache};
use twilight_gateway::{cluster::Cluster, Event};
use twilight_http::Client as HttpClient;
use twilight_model::gateway::{
    payload::update_status::UpdateStatusInfo,
    presence::{Activity, ActivityType, Status},
    Intents,
};

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_log::LogTracer::init()?;
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    let bind = env::var("ACCORD_BIND").unwrap_or_else(|_| String::from("localhost:8181"));
    let token = env::var("DISCORD_TOKEN").expect("FATAL: missing env: DISCORD_TOKEN");
    let target_base = env::var("ACCORD_TARGET").expect("FATAL: missing env: ACCORD_TARGET");
    let command_match = env::var("ACCORD_COMMAND_MATCH").ok();
    let command_parse = env::var("ACCORD_COMMAND_PARSE").ok();
    let target = Arc::new(raccord::Client::new(
        target_base,
        command_match,
        command_parse,
    ));

    let (s, r) = unbounded();

    match main_forward(token, target, r)
        .join(main_reverse(bind, s))
        .await
    {
        (Ok(_), Ok(_)) => Ok(()),
        (Ok(_), Err(e)) => Err(e),
        (Err(e), Ok(_)) => Err(e),
        (Err(e), Err(f)) => {
            eprintln!("{}", e);
            Err(f)
        }
    }
}

async fn main_forward(
    token: String,
    target: Arc<raccord::Client>,
    ghosts: Receiver<(u64, Event)>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut update_status = None;
    if let Ok(mut connecting_res) = target.get(raccord::Connecting)?.await {
        if connecting_res.status().is_success() {
            let content_type = connecting_res
                .headers()
                .get("content-type")
                .and_then(|s| s.to_str().ok())
                .and_then(|s| mime::Mime::from_str(s).ok())
                .unwrap_or(mime::APPLICATION_OCTET_STREAM);

            if content_type == mime::APPLICATION_JSON {
                let presence: raccord::Presence = connecting_res.json()?;

                update_status = Some(UpdateStatusInfo {
                    afk: presence.afk.unwrap_or(true),
                    since: presence.since,
                    status: presence.status.unwrap_or(Status::Online),
                    game: presence.activity.map(|activity| {
                        let (kind, name) = match activity {
                            raccord::Activity::Playing { name } => (ActivityType::Playing, name),
                            raccord::Activity::Streaming { name } => {
                                (ActivityType::Streaming, name)
                            }
                            raccord::Activity::Listening { name } => {
                                (ActivityType::Listening, name)
                            }
                            raccord::Activity::Watching { name } => (ActivityType::Watching, name),
                            raccord::Activity::Custom { name } => (ActivityType::Custom, name),
                        };

                        Activity {
                            application_id: None,
                            assets: None,
                            created_at: None,
                            details: None,
                            emoji: None,
                            flags: None,
                            id: None,
                            instance: None,
                            party: None,
                            secrets: None,
                            state: None,
                            timestamps: None,
                            url: None,
                            kind,
                            name,
                        }
                    }),
                });
            }
        }
    }

    // TODO: env var control for intents (notably for privileged intents)
    let mut config = Cluster::builder(&token)
        .intents(Intents::DIRECT_MESSAGES | Intents::GUILD_MESSAGES | Intents::GUILD_MEMBERS);

    if let Some(presence) = update_status {
        config = config.presence(presence);
    }

    let cluster = config.build().await?;

    let cluster_spawn = cluster.clone();
    spawn(async move {
        cluster_spawn.up().await;
    });

    let http = HttpClient::new(&token);

    let cache = InMemoryCache::builder()
        .event_types(
            EventType::MESSAGE_CREATE
                | EventType::MESSAGE_DELETE
                | EventType::MESSAGE_DELETE_BULK
                | EventType::MESSAGE_UPDATE
                | EventType::MEMBER_ADD
                | EventType::MEMBER_CHUNK
                | EventType::MEMBER_UPDATE
                | EventType::MEMBER_REMOVE,
        )
        .build();

    let solids = cluster.events();
    let mut events = solids.merge(ghosts);

    while let Some((shard_id, event)) = events.next().await {
        cache.update(&event);
        spawn(handle_event(target.clone(), shard_id, event, http.clone()));
    }

    Ok(())
}

async fn main_reverse(
    bind: String,
    ghosts: Sender<(u64, Event)>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    use tide::{Request, Response, StatusCode};
    use twilight_model::{channel::Message, gateway::payload::MessageCreate};

    #[derive(Clone, Debug)]
    struct State {
        pub ghosts: Sender<(u64, Event)>,
    }

    let mut app = Server::with_state(State { ghosts });
    app.with(TraceMiddleware::new());

    app.at("/ghost/server/:server/channel/:channel/message")
        .post(|mut req: Request<State>| async move {
            // TODO: handle text/plain body
            let message: raccord::ServerMessage = req.body_json().await?;
            // TODO: log(warn) if :channel != message.channel etc
            let message: Message = (&message).into();
            let event = Event::MessageCreate(Box::new(MessageCreate(message)));
            req.state().ghosts.send((0, event)).await?;
            Ok(Response::new(StatusCode::NoContent))
        });

    app.at("/ghost/direct/channel/:channel/message")
        .post(|mut req: Request<State>| async move {
            // TODO: handle text/plain body
            let message: raccord::DirectMessage = req.body_json().await?;
            // TODO: log(warn) if :channel != message.channel
            let message: Message = (&message).into();
            let event = Event::MessageCreate(Box::new(MessageCreate(message)));
            req.state().ghosts.send((0, event)).await?;
            Ok(Response::new(StatusCode::NoContent))
        });

    app.listen(bind).await?;
    Ok(())
}
