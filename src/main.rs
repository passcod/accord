use isahc::ResponseExt;
use std::{env, error::Error, str::FromStr, sync::Arc};
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
    model::{
        gateway::GatewayIntents,
        id::{ChannelId, GuildId, RoleId, UserId},
    },
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
    use isahc::{
        config::{Configurable, RedirectPolicy},
        http::request::{Builder as RequestBuilder, Request},
        HttpClient, ResponseFuture,
    };
    use regex::Regex;
    use serde::{Deserialize, Serialize};
    use std::{fmt, time::Duration};
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

    pub struct Client {
        base: String,
        command_regex: Option<(Regex, Option<Regex>)>,
        client: HttpClient,
    }

    impl Client {
        pub fn new(
            base: String,
            command_match: Option<String>,
            command_parse: Option<String>,
        ) -> Self {
            let client = HttpClient::builder()
                .default_header("accord-version", env!("CARGO_PKG_VERSION"))
                .redirect_policy(RedirectPolicy::Limit(8))
                .auto_referer()
                .tcp_keepalive(Duration::from_secs(15))
                .tcp_nodelay()
                .build()
                .expect("failed to create http client");
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

        pub fn post<S: Sendable>(&self, payload: S) -> ResponseFuture {
            info!("sending {}", std::any::type_name::<S>());
            let req = payload
                .customise(
                    Request::post(format!("{}{}", self.base, payload.url()))
                        .header("content-type", "application/json"),
                )
                .body(serde_json::to_vec(&payload).expect("failed to serialize payload"))
                .expect("failed to create request");
            self.client.send_async(req)
        }
    }

    pub trait Sendable: Serialize {
        fn url(&self) -> String;

        fn customise(&self, req: RequestBuilder) -> RequestBuilder {
            req
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

    #[derive(Clone, Debug, Serialize)]
    pub struct ServerJoin(pub Member);

    impl Sendable for ServerJoin {
        fn url(&self) -> String {
            format!("/server/{}/join/{}", self.0.server_id, self.0.user.id)
        }

        fn customise(&self, mut req: RequestBuilder) -> RequestBuilder {
            req = req
                .header("accord-server-id", self.0.server_id)
                .header("accord-member-id", self.0.user.id)
                .header("accord-member-name", &self.0.user.name);

            if let Some(ref pseud) = self.0.pseudonym {
                req = req.header("accord-member-pseudonym", pseud);
            }

            if let Some(ref roles) = self.0.roles {
                for role in roles {
                    req = req.header("accord-member-role-ids", *role);
                }
            }

            req
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

        fn customise(&self, mut req: RequestBuilder) -> RequestBuilder {
            req = req
                .header("accord-message-id", self.id)
                .header("accord-server-id", self.server_id)
                .header("accord-channel-type", "text")
                .header("accord-channel-id", self.channel_id)
                .header(
                    "accord-author-type",
                    if self.author.user.bot { "bot" } else { "user" },
                )
                .header("accord-author-id", self.author.user.id)
                .header("accord-author-name", &self.author.user.name)
                .header("accord-content-length", self.content.len());

            if let Some(ref pseud) = self.author.pseudonym {
                req = req.header("accord-author-pseudonym", pseud);
            }

            if let Some(ref roles) = self.author.roles {
                for role in roles {
                    req = req.header("accord-author-role-ids", *role);
                }
            }

            for flag in &self.flags {
                req = req.header("accord-message-flags", flag.to_string());
            }

            if !self.attachments.is_empty() {
                req = req.header("accord-has-attachments", self.attachments.len());
            }

            if !self.embeds.is_empty() {
                req = req.header("accord-has-embeds", self.embeds.len());
            }

            if !self.reactions.is_empty() {
                req = req.header("accord-has-reactions", self.reactions.len());
            }

            req
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

        fn customise(&self, mut req: RequestBuilder) -> RequestBuilder {
            req = req
                .header("accord-message-id", self.id)
                .header("accord-channel-type", "direct")
                .header("accord-channel-id", self.channel_id)
                .header(
                    "accord-author-type",
                    if self.author.bot { "bot" } else { "user" }.to_string(),
                )
                .header("accord-author-id", self.author.id)
                .header("accord-author-name", &self.author.name)
                .header("accord-content-length", self.content.len());

            for flag in &self.flags {
                req = req.header("accord-message-flags", flag.to_string());
            }

            if !self.attachments.is_empty() {
                req = req.header("accord-has-attachments", self.attachments.len());
            }

            if !self.embeds.is_empty() {
                req = req.header("accord-has-embeds", self.embeds.len());
            }

            if !self.reactions.is_empty() {
                req = req.header("accord-has-reactions", self.reactions.len());
            }

            req
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

        fn customise(&self, req: RequestBuilder) -> RequestBuilder {
            self.message.customise(req)
        }
    }

    #[derive(Clone, Debug, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub enum Act {
        CreateMessage {
            content: String,
            channel_id: Option<u64>,
        },
        AssignRole {
            role_id: u64,
            user_id: u64,
            server_id: Option<u64>,
            reason: Option<String>,
        },
        RemoveRole {
            role_id: u64,
            user_id: u64,
            server_id: Option<u64>,
            reason: Option<String>,
        },
    }
}

async fn handle_event(
    target: Arc<raccord::Client>,
    event: (u64, Event),
    http: HttpClient,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match event {
        (_, Event::MessageCreate(message)) if message.guild_id.is_some() => {
            let msg = raccord::ServerMessage::from(&**message);
            let mut res = if let Some(command) = target.parse_command(&msg.content) {
                target.post(raccord::Command {
                    command,
                    message: msg,
                })
            } else {
                target.post(msg)
            }
            .await?;

            dbg!(&res);

            let status = res.status();
            if status.is_informational() {
                todo!("log(error) unhandled 1xx code");
            }

            if status == 204 || status == 404 {
                // no content, no action
                return Ok(());
            }

            if status.is_redirection() {
                match status.into() {
                    300 => todo!("multiple choice design"),
                    301 | 302 | 303 | 307 | 308 => {
                        unreachable!("redirects should be handled by curl")
                    }
                    304 => todo!("response caching"),
                    305 | 306 => todo!("log(error) proxy redirections as unsupported"),
                    _ => todo!("log(error) invalid 3xx code"),
                }
            }

            if status.is_client_error() || status.is_server_error() {
                todo!("http error reporting");
            }

            if !status.is_success() {
                todo!("log(error) invalid response status");
            }

            let content_type = res
                .headers()
                .get("content-type")
                .and_then(|s| s.to_str().ok())
                .and_then(|s| mime::Mime::from_str(s).ok())
                .unwrap_or(mime::APPLICATION_OCTET_STREAM);

            match (content_type.type_(), content_type.subtype()) {
                (mime::APPLICATION, mime::JSON) => {
                    let act: raccord::Act = res.json()?;
                    let default_server_id = res
                        .headers()
                        .get("accord-server-id")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| u64::from_str(s).ok())
                        .map(GuildId)
                        .unwrap_or(message.guild_id.unwrap());
                    let default_channel_id = res
                        .headers()
                        .get("accord-channel-id")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| u64::from_str(s).ok())
                        .map(ChannelId)
                        .unwrap_or(message.channel_id);

                    match act {
                        raccord::Act::CreateMessage {
                            content,
                            channel_id,
                        } => {
                            let channel_id =
                                channel_id.map(ChannelId).unwrap_or(default_channel_id);
                            http.create_message(channel_id).content(content)?.await?;
                        }
                        raccord::Act::AssignRole {
                            role_id,
                            user_id,
                            server_id,
                            reason,
                        } => {
                            let server_id = server_id.map(GuildId).unwrap_or(default_server_id);

                            let mut add =
                                http.add_role(server_id, UserId(user_id), RoleId(role_id));

                            if let Some(text) = reason {
                                add = add.reason(text);
                            }

                            add.await?;
                        }
                        raccord::Act::RemoveRole {
                            role_id,
                            user_id,
                            server_id,
                            reason,
                        } => {
                            let server_id = server_id.map(GuildId).unwrap_or(default_server_id);

                            let mut rm = http.remove_guild_member_role(
                                server_id,
                                UserId(user_id),
                                RoleId(role_id),
                            );

                            if let Some(text) = reason {
                                rm = rm.reason(text);
                            }

                            rm.await?;
                        }
                        _ => todo!("handle other acts"),
                    }
                }
                (mime::TEXT, mime::PLAIN) => {
                    let reply = res.text().expect("todo: log(error) failed to decode text");
                    let channel_id = res
                        .headers()
                        .get("accord-channel-id")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| u64::from_str(s).ok())
                        .map(ChannelId)
                        .unwrap_or(message.channel_id);

                    http.create_message(channel_id).content(reply)?.await?;
                }
                (t, s) => todo!("log(warn) unhandled content-type {}/{}", t, s),
            }
        }
        (_, Event::MessageCreate(msg)) => {
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

            //http.create_message(msg.channel_id).content("beep")?.await?;
        }
        (_, Event::MemberAdd(mem)) => {
            let member = raccord::Member::from(&**mem);
            let res = target.post(raccord::ServerJoin(member)).await?;
        }
        (shard, Event::ShardConnected(_)) => {
            info!("connected on shard {}", shard);
            let res = target.post(raccord::Connected { shard }).await?;
        }
        _ => {}
    }

    Ok(())
}
