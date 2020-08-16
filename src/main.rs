use std::{env, error::Error, sync::Arc};
use tokio::stream::StreamExt;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use twilight::{
    cache::{
        twilight_cache_inmemory::config::{EventType, InMemoryConfigBuilder},
        InMemoryCache,
    },
    gateway::{
        cluster::{config::ShardScheme, Cluster, ClusterConfig},
        Event,
    },
    http::Client as HttpClient,
    model::gateway::GatewayIntents,
};

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_log::LogTracer::init()?;
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    let token = env::var("DISCORD_TOKEN")?;
    let target_base = env::var("ACCORD_TARGET")?;
    let command_match = env::var("ACCORD_COMMAND_MATCH").ok();
    let command_parse = env::var("ACCORD_COMMAND_PARSE").ok();
    let target = Arc::new(raccord::Client::new(
        target_base,
        command_match,
        command_parse,
    ));

    // This is also the default.
    let scheme = ShardScheme::Auto;

    let config = ClusterConfig::builder(&token)
        .shard_scheme(scheme)
        // Use intents to only listen to GUILD_MESSAGES events
        .intents(Some(
            GatewayIntents::GUILD_MESSAGES | GatewayIntents::DIRECT_MESSAGES,
        ))
        .build();

    // Start up the cluster
    let cluster = Cluster::new(config).await?;

    let cluster_spawn = cluster.clone();

    tokio::spawn(async move {
        cluster_spawn.up().await;
    });

    // The http client is separate from the gateway,
    // so startup a new one
    let http = HttpClient::new(&token);

    // Since we only care about messages, make the cache only
    // cache message related events
    let cache_config = InMemoryConfigBuilder::new()
        .event_types(
            EventType::MESSAGE_CREATE
                | EventType::MESSAGE_DELETE
                | EventType::MESSAGE_DELETE_BULK
                | EventType::MESSAGE_UPDATE,
        )
        .build();
    let cache = InMemoryCache::from(cache_config);

    let mut events = cluster.events().await;
    // Startup an event loop for each event in the event stream
    while let Some(event) = events.next().await {
        // Update the cache
        cache.update(&event.1).await.expect("Cache failed, OhNoe");

        // Spawn a new task to handle the event
        tokio::spawn(handle_event(target.clone(), event, http.clone()));
    }

    Ok(())
}

mod raccord {
    use http_client::h1::H1Client as C;
    use http_types::headers::HeaderValue;
    use regex::Regex;
    use serde::Serialize;
    use std::{convert::TryFrom, fmt};
    use surf::Request;
    use tracing::info;
    use twilight::model::{
        channel::{
            embed::Embed,
            message::{
                Message as DisMessage, MessageApplication, MessageFlags as DisMessageFlags,
                MessageReaction, MessageType,
            },
            Attachment,
        },
        guild::Member as DisMember,
        user::User as DisUser,
    };

    // TODO: probably need a pool of clients rather than Arcing one?
    pub struct Client {
        base: String,
        command_regex: Option<(Regex, Option<Regex>)>,
        client: surf::Client<C>,
    }

    impl Client {
        pub fn new(
            base: String,
            command_match: Option<String>,
            command_parse: Option<String>,
        ) -> Self {
            let client = surf::Client::new();
            let command_match_regex = command_match
                .as_ref()
                .map(|s| Regex::new(s).expect("bad regex: ACCORD_COMMAND_MATCH"));
            let command_parse_regex = command_parse
                .as_ref()
                .map(|s| Regex::new(s).expect("bad regex: ACCORD_COMMAND_PARSE"));

            let command_regex = command_match_regex.map(|mx| (mx, command_parse_regex));

            Self {
                base,
                command_regex,
                client,
            }
        }

        pub fn parse_command(&self, content: &str) -> Option<Vec<String>> {
            self.command_regex.as_ref().and_then(|(matcher, parser)| {
                if !matcher.is_match(content) {
                    None
                } else if let Some(px) = parser {
                    Some(
                        px.captures_iter(content)
                            .map(|captures| -> Vec<String> {
                                captures
                                    .iter()
                                    .skip(1)
                                    .flat_map(|m| m.map(|m| m.as_str().to_string()))
                                    .collect()
                            })
                            .flatten()
                            .collect(),
                    )
                } else {
                    Some(Vec::new())
                }
            })
        }

        fn add_headers(mut req: Request<C>, headers: Vec<(&str, Vec<String>)>) -> Request<C> {
            for (name, values) in headers {
                req = req.set_header(
                    format!("accord-{}", name).as_str(),
                    values
                        .iter()
                        .map(|v| HeaderValue::try_from(v.as_str()).expect("invalid header value"))
                        .collect::<Vec<HeaderValue>>()
                        .as_slice(),
                );
            }

            req
        }

        pub fn post<S: Sendable>(&self, payload: S) -> Request<C> {
            info!("sending {}", std::any::type_name::<S>());
            Self::add_headers(
                self.client.post(format!("{}{}", self.base, payload.url())),
                payload.headers(),
            )
            .body_json(&payload)
            .expect("failed to serialize payload")
        }
    }

    pub trait Sendable: Serialize {
        fn url(&self) -> String;

        fn headers(&self) -> Vec<(&str, Vec<String>)> {
            Vec::new()
        }
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct Connected {
        pub shard: u64,
    }

    impl Sendable for Connected {
        fn url(&self) -> String {
            "/hello/discord".to_string()
        }
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct User {
        pub id: u64,
        pub name: String,
        pub bot: bool,
        pub roles: Option<Vec<u64>>,
    }

    impl From<&DisUser> for User {
        fn from(dis: &DisUser) -> Self {
            Self {
                id: dis.id.0,
                name: dis.name.clone(),
                bot: dis.bot,
                roles: None,
            }
        }
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct Member {
        pub user: User,
        pub server_id: u64,
        pub roles: Option<Vec<u64>>,
        pub pseudonym: Option<String>,
    }

    impl From<&DisMessage> for Member {
        fn from(dis: &DisMessage) -> Self {
            Self {
                user: User::from(&dis.author),
                server_id: dis.guild_id.map(|g| g.0).unwrap_or_default(),
                roles: dis
                    .member
                    .as_ref()
                    .map(|mem| mem.roles.iter().map(|role| role.0).collect()),
                pseudonym: None,
            }
        }
    }

    impl From<&DisMember> for Member {
        fn from(dis: &DisMember) -> Self {
            Self {
                user: User::from(&dis.user),
                server_id: dis.guild_id.0,
                roles: Some(dis.roles.iter().map(|role| role.0).collect()),
                pseudonym: dis.nick.clone(),
            }
        }
    }

    #[derive(Clone, Copy, Debug, Serialize)]
    pub enum MessageFlag {
        Crossposted,
        IsCrosspost,
        SuppressEmbeds,
        SourceMessageDeleted,
        Urgent,
    }

    impl MessageFlag {
        pub fn from_discord(dis: DisMessageFlags) -> Vec<Self> {
            let mut flags = Vec::with_capacity(5);
            if dis.contains(DisMessageFlags::CROSSPOSTED) {
                flags.push(Self::Crossposted);
            }
            if dis.contains(DisMessageFlags::IS_CROSSPOST) {
                flags.push(Self::IsCrosspost);
            }
            if dis.contains(DisMessageFlags::SUPPRESS_EMBEDS) {
                flags.push(Self::SuppressEmbeds);
            }
            if dis.contains(DisMessageFlags::SOURCE_MESSAGE_DELETED) {
                flags.push(Self::SourceMessageDeleted);
            }
            if dis.contains(DisMessageFlags::URGENT) {
                flags.push(Self::Urgent);
            }
            flags
        }
    }

    impl fmt::Display for MessageFlag {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            use MessageFlag::*;

            write!(
                f,
                "{}",
                match self {
                    Crossposted => "crossposted",
                    IsCrosspost => "is-crosspost",
                    SuppressEmbeds => "suppress-embeds",
                    SourceMessageDeleted => "source-message-deleted",
                    Urgent => "urgent",
                }
            )
        }
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct ServerMessage {
        pub id: u64,
        pub server_id: u64,
        pub channel_id: u64,
        pub author: Member,

        pub timestamp_created: String,
        pub timestamp_edited: Option<String>,

        pub kind: MessageType,
        pub content: String,

        pub attachments: Vec<Attachment>,
        pub embeds: Vec<Embed>,
        pub reactions: Vec<MessageReaction>,

        pub application: Option<MessageApplication>,
        pub flags: Vec<MessageFlag>,
    }

    impl Sendable for ServerMessage {
        fn url(&self) -> String {
            format!(
                "/server/{}/channel/{}/message",
                self.server_id, self.channel_id
            )
        }

        fn headers(&self) -> Vec<(&str, Vec<String>)> {
            let mut h = vec![
                ("message-id", vec![self.id.to_string()]),
                ("server-id", vec![self.server_id.to_string()]),
                ("channel-id", vec![self.channel_id.to_string()]),
                ("author-id", vec![self.author.user.id.to_string()]),
                ("author-name", vec![self.author.user.name.clone()]),
                ("content-length", vec![self.content.len().to_string()]),
            ];

            if let Some(ref pseud) = self.author.pseudonym {
                h.push(("author-pseudonym", vec![pseud.clone()]));
            }

            if let Some(ref roles) = self.author.roles {
                if !roles.is_empty() {
                    h.push((
                        "author-role-ids",
                        roles.iter().map(|role| role.to_string()).collect(),
                    ));
                }
            }

            if !self.flags.is_empty() {
                h.push((
                    "message-flags",
                    self.flags.iter().map(ToString::to_string).collect(),
                ));
            }

            if !self.attachments.is_empty() {
                h.push(("has-attachments", vec![self.attachments.len().to_string()]));
            }

            if !self.embeds.is_empty() {
                h.push(("has-embeds", vec![self.embeds.len().to_string()]));
            }

            if !self.reactions.is_empty() {
                h.push(("has-reactions", vec![self.reactions.len().to_string()]));
            }

            h
        }
    }

    impl From<&DisMessage> for ServerMessage {
        /// Convert from a Discord message to a Raccord ServerMessage
        ///
        /// # Panics
        ///
        /// Will panic if there's no `guild_id`.
        fn from(dis: &DisMessage) -> Self {
            Self {
                id: dis.id.0,
                server_id: dis.guild_id.unwrap().0,
                channel_id: dis.channel_id.0,
                author: dis.into(),

                timestamp_created: dis.timestamp.clone(),
                timestamp_edited: dis.edited_timestamp.clone(),

                kind: dis.kind,
                content: dis.content.clone(),

                attachments: dis.attachments.clone(),
                embeds: dis.embeds.clone(),
                reactions: dis.reactions.clone(),

                application: dis.application.clone(),
                flags: dis.flags.map(MessageFlag::from_discord).unwrap_or_default(),
            }
        }
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct DirectMessage {
        pub id: u64,
        pub channel_id: u64,
        pub author: User,

        pub timestamp_created: String,
        pub timestamp_edited: Option<String>,

        pub kind: MessageType,
        pub content: String,

        pub attachments: Vec<Attachment>,
        pub embeds: Vec<Embed>,
        pub reactions: Vec<MessageReaction>,

        pub application: Option<MessageApplication>,
        pub flags: Vec<MessageFlag>,
    }

    impl Sendable for DirectMessage {
        fn url(&self) -> String {
            format!("/direct/{}/message", self.channel_id)
        }

        fn headers(&self) -> Vec<(&str, Vec<String>)> {
            let mut h = vec![
                ("message-id", vec![self.id.to_string()]),
                ("direct-message", vec!["true".to_string()]),
                ("channel-id", vec![self.channel_id.to_string()]),
                ("author-id", vec![self.author.id.to_string()]),
                ("author-name", vec![self.author.name.clone()]),
                ("content-length", vec![self.content.len().to_string()]),
            ];

            if !self.flags.is_empty() {
                h.push((
                    "message-flags",
                    self.flags.iter().map(ToString::to_string).collect(),
                ));
            }

            if !self.attachments.is_empty() {
                h.push(("has-attachments", vec![self.attachments.len().to_string()]));
            }

            if !self.embeds.is_empty() {
                h.push(("has-embeds", vec![self.embeds.len().to_string()]));
            }

            if !self.reactions.is_empty() {
                h.push(("has-reactions", vec![self.reactions.len().to_string()]));
            }

            h
        }
    }

    impl From<&DisMessage> for DirectMessage {
        fn from(dis: &DisMessage) -> Self {
            Self {
                id: dis.id.0,
                channel_id: dis.channel_id.0,
                author: User::from(&dis.author),

                timestamp_created: dis.timestamp.clone(),
                timestamp_edited: dis.edited_timestamp.clone(),

                kind: dis.kind,
                content: dis.content.clone(),

                attachments: dis.attachments.clone(),
                embeds: dis.embeds.clone(),
                reactions: dis.reactions.clone(),

                application: dis.application.clone(),
                flags: dis.flags.map(MessageFlag::from_discord).unwrap_or_default(),
            }
        }
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct Command<M: Sendable> {
        pub command: Vec<String>,
        pub message: M,
    }

    impl<S: Sendable> Sendable for Command<S> {
        fn url(&self) -> String {
            format!("/command/{}", self.command.join("/"))
        }

        fn headers(&self) -> Vec<(&str, Vec<String>)> {
            self.message.headers()
        }
    }
}

async fn handle_event(
    target: Arc<raccord::Client>,
    event: (u64, Event),
    http: HttpClient,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match event {
        (_, Event::MessageCreate(msg)) => {
            if msg.guild_id.is_some() {
                let msg = raccord::ServerMessage::from(&**msg);
                let res = if let Some(command) = target.parse_command(&msg.content) {
                    target.post(raccord::Command {
                        command,
                        message: msg,
                    })
                } else {
                    target.post(msg)
                }
                .await?;
            } else {
                let msg = raccord::DirectMessage::from(&**msg);
                let res = if let Some(command) = target.parse_command(&msg.content) {
                    target.post(raccord::Command {
                        command,
                        message: msg,
                    })
                } else {
                    target.post(msg)
                }
                .await?;
            }

            //http.create_message(msg.channel_id).content("beep")?.await?;
        }
        (shard, Event::ShardConnected(_)) => {
            info!("connected on shard {}", shard);
            let res = target.post(raccord::Connected { shard }).await?;
        }
        _ => {}
    }

    Ok(())
}
