use isahc::{
	config::{Configurable, RedirectPolicy},
	http::request::{Builder as RequestBuilder, Request},
	HttpClient, ResponseFuture,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt, time::Duration};
use tracing::{info, trace};
use twilight_model::{
	channel::{
		embed::Embed,
		message::{
			Message as DisMessage, MessageApplication, MessageFlags as DisMessageFlags,
			MessageReaction, MessageType as DisMessageType,
		},
		Attachment,
	},
	gateway::presence::Status,
	guild::{Member as DisMember, PartialMember},
	id::{ChannelId, GuildId, MessageId, RoleId, UserId},
	user::User as DisUser,
};

pub struct Client {
	base: String,
	command_regex: Option<(Regex, Option<Regex>)>,
	client: HttpClient,
}

impl Client {
	pub fn new(base: String, command_match: Option<String>, command_parse: Option<String>) -> Self {
		let client = HttpClient::builder()
			.default_header("accord-version", env!("CARGO_PKG_VERSION"))
			.redirect_policy(RedirectPolicy::Limit(8))
			.auto_referer()
			.tcp_keepalive(Duration::from_secs(15))
			.tcp_nodelay()
			.build()
			.expect("FATAL: failed to create http client");
		let command_match_regex = command_match
			.as_ref()
			.map(|s| Regex::new(s).expect("FATAL: bad regex: ACCORD_COMMAND_MATCH"));
		let command_parse_regex = command_parse
			.as_ref()
			.map(|s| Regex::new(s).expect("FATAL: bad regex: ACCORD_COMMAND_PARSE"));

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

	pub fn get<S: Sendable>(
		&self,
		payload: S,
	) -> Result<ResponseFuture, Box<dyn Error + Send + Sync>> {
		trace!(
			payload_type = std::any::type_name::<S>(),
			"constructing request"
		);
		let req = payload
			.customise(
				Request::get(format!("{}{}", self.base, payload.url()))
					.header("content-type", "application/json"),
			)
			.body(())?;
		info!(
			to = payload.url().as_str(),
			"sending {}",
			std::any::type_name::<S>()
		);
		Ok(self.client.send_async(req))
	}

	pub fn post<S: Sendable>(
		&self,
		payload: S,
	) -> Result<ResponseFuture, Box<dyn Error + Send + Sync>> {
		trace!(
			payload_type = std::any::type_name::<S>(),
			"constructing request"
		);
		let req = payload
			.customise(
				Request::post(format!("{}{}", self.base, payload.url()))
					.header("content-type", "application/json"),
			)
			.body(serde_json::to_vec(&payload)?)?;
		info!(
			to = payload.url().as_str(),
			"sending {}",
			std::any::type_name::<S>()
		);
		Ok(self.client.send_async(req))
	}
}

pub trait Sendable: Serialize {
	fn url(&self) -> String;

	fn customise(&self, req: RequestBuilder) -> RequestBuilder {
		req
	}
}

#[derive(Clone, Debug, Serialize)]
pub struct Connecting;

impl Sendable for Connecting {
	fn url(&self) -> String {
		"/discord/connecting".to_string()
	}
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Activity {
	Playing { name: String },
	Streaming { name: String },
	Listening { name: String },
	Watching { name: String },
	Custom { name: String },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Presence {
	pub afk: Option<bool>,
	pub activity: Option<Activity>,
	pub since: Option<u64>,
	pub status: Option<Status>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Connected {
	pub shard: u64,
}

impl Sendable for Connected {
	fn url(&self) -> String {
		"/discord/connected".to_string()
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
	pub id: u64,
	pub name: String,
	#[serde(default)]
	pub discriminator: String,
	#[serde(default)]
	pub bot: bool,
}

impl From<&DisUser> for User {
	fn from(dis: &DisUser) -> Self {
		Self {
			id: dis.id.0,
			discriminator: dis.discriminator.clone(),
			name: dis.name.clone(),
			bot: dis.bot,
		}
	}
}

impl From<&User> for DisUser {
	fn from(rac: &User) -> Self {
		Self {
			id: UserId(rac.id),
			discriminator: rac.discriminator.clone(),
			name: rac.name.clone(),
			bot: rac.bot,

			avatar: Default::default(),
			email: Default::default(),
			flags: Default::default(),
			locale: Default::default(),
			mfa_enabled: Default::default(),
			premium_type: Default::default(),
			public_flags: Default::default(),
			system: Default::default(),
			verified: Default::default(),
		}
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Member {
	pub user: User,
	pub server_id: u64,
	#[serde(default)]
	pub roles: Option<Vec<u64>>,
	#[serde(default)]
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
			pseudonym: dis.member.as_ref().and_then(|mem| mem.nick.clone()),
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

impl From<&Member> for PartialMember {
	fn from(rac: &Member) -> Self {
		Self {
			roles: rac
				.roles
				.as_ref()
				.map(|v| v.into_iter().map(|r| RoleId(*r)).collect())
				.unwrap_or_default(),
			nick: rac.pseudonym.clone(),

			deaf: Default::default(),
			mute: Default::default(),
			joined_at: Default::default(),
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
			.header("accord-member-name", &escape(&self.0.user.name));

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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
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

	pub fn to_discord(rac: &[Self]) -> Option<DisMessageFlags> {
		if rac.is_empty() {
			return None;
		}

		let mut flags = DisMessageFlags::empty();
		if rac.contains(&MessageFlag::Crossposted) {
			flags.insert(DisMessageFlags::CROSSPOSTED);
		}
		if rac.contains(&MessageFlag::IsCrosspost) {
			flags.insert(DisMessageFlags::IS_CROSSPOST);
		}
		if rac.contains(&MessageFlag::SuppressEmbeds) {
			flags.insert(DisMessageFlags::SUPPRESS_EMBEDS);
		}
		if rac.contains(&MessageFlag::SourceMessageDeleted) {
			flags.insert(DisMessageFlags::SOURCE_MESSAGE_DELETED);
		}
		if rac.contains(&MessageFlag::Urgent) {
			flags.insert(DisMessageFlags::URGENT);
		}

		Some(flags)
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
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MessageType {
	Regular,
	RecipientAdd,
	RecipientRemove,
	Call,
	ChannelNameChange,
	ChannelIconChange,
	ChannelMessagePinned,
	GuildMemberJoin,
	UserPremiumSub,
	UserPremiumSubTier1,
	UserPremiumSubTier2,
	UserPremiumSubTier3,
	ChannelFollowAdd,
	GuildDiscoveryDisqualified,
	GuildDiscoveryRequalified,
	Reply,
}

impl Default for MessageType {
	fn default() -> Self {
		Self::Regular
	}
}

impl From<DisMessageType> for MessageType {
	fn from(dis: DisMessageType) -> Self {
		use MessageType::*;
		match dis {
			DisMessageType::Regular => Regular,
			DisMessageType::RecipientAdd => RecipientAdd,
			DisMessageType::RecipientRemove => RecipientRemove,
			DisMessageType::Call => Call,
			DisMessageType::ChannelNameChange => ChannelNameChange,
			DisMessageType::ChannelIconChange => ChannelIconChange,
			DisMessageType::ChannelMessagePinned => ChannelMessagePinned,
			DisMessageType::GuildMemberJoin => GuildMemberJoin,
			DisMessageType::UserPremiumSub => UserPremiumSub,
			DisMessageType::UserPremiumSubTier1 => UserPremiumSubTier1,
			DisMessageType::UserPremiumSubTier2 => UserPremiumSubTier2,
			DisMessageType::UserPremiumSubTier3 => UserPremiumSubTier3,
			DisMessageType::ChannelFollowAdd => ChannelFollowAdd,
			DisMessageType::GuildDiscoveryDisqualified => GuildDiscoveryDisqualified,
			DisMessageType::GuildDiscoveryRequalified => GuildDiscoveryRequalified,
			DisMessageType::Reply => Reply,
		}
	}
}
impl From<MessageType> for DisMessageType {
	fn from(rac: MessageType) -> Self {
		use DisMessageType::*;
		match rac {
			MessageType::Regular => Regular,
			MessageType::RecipientAdd => RecipientAdd,
			MessageType::RecipientRemove => RecipientRemove,
			MessageType::Call => Call,
			MessageType::ChannelNameChange => ChannelNameChange,
			MessageType::ChannelIconChange => ChannelIconChange,
			MessageType::ChannelMessagePinned => ChannelMessagePinned,
			MessageType::GuildMemberJoin => GuildMemberJoin,
			MessageType::UserPremiumSub => UserPremiumSub,
			MessageType::UserPremiumSubTier1 => UserPremiumSubTier1,
			MessageType::UserPremiumSubTier2 => UserPremiumSubTier2,
			MessageType::UserPremiumSubTier3 => UserPremiumSubTier3,
			MessageType::ChannelFollowAdd => ChannelFollowAdd,
			MessageType::GuildDiscoveryDisqualified => GuildDiscoveryDisqualified,
			MessageType::GuildDiscoveryRequalified => GuildDiscoveryRequalified,
			MessageType::Reply => Reply,
		}
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerMessage {
	pub id: u64,
	pub server_id: u64,
	pub channel_id: u64,
	pub author: Member,

	pub timestamp_created: String,
	#[serde(default)]
	pub timestamp_edited: Option<String>,

	#[serde(default)]
	pub kind: MessageType,
	pub content: String,

	#[serde(default)]
	pub attachments: Vec<Attachment>,
	#[serde(default)]
	pub embeds: Vec<Embed>,
	#[serde(default)]
	pub reactions: Vec<MessageReaction>,

	#[serde(default)]
	pub application: Option<MessageApplication>,
	#[serde(default)]
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
			.header("accord-author-name", &escape(&self.author.user.name))
			.header("accord-content-length", self.content.len());

		if let Some(ref pseud) = self.author.pseudonym {
			req = req.header("accord-author-pseudonym", escape(pseud));
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

			kind: dis.kind.into(),
			content: dis.content.clone(),

			attachments: dis.attachments.clone(),
			embeds: dis.embeds.clone(),
			reactions: dis.reactions.clone(),

			application: dis.application.clone(),
			flags: dis.flags.map(MessageFlag::from_discord).unwrap_or_default(),
		}
	}
}

impl From<&ServerMessage> for DisMessage {
	/// Convert from a Raccord ServerMessage to a Discord Message
	fn from(rac: &ServerMessage) -> Self {
		Self {
			id: MessageId(rac.id),
			guild_id: Some(GuildId(rac.server_id)),
			channel_id: ChannelId(rac.channel_id),
			author: (&rac.author.user).into(),
			member: Some((&rac.author).into()),

			timestamp: rac.timestamp_created.clone(),
			edited_timestamp: rac.timestamp_edited.clone(),

			kind: rac.kind.into(),
			content: rac.content.clone(),

			attachments: rac.attachments.clone(),
			embeds: rac.embeds.clone(),
			reactions: rac.reactions.clone(),

			application: rac.application.clone(),
			flags: MessageFlag::to_discord(&rac.flags),

			activity: Default::default(),
			mention_channels: Default::default(),
			mention_everyone: Default::default(),
			mention_roles: Default::default(),
			mentions: Default::default(),
			stickers: Default::default(),
			tts: Default::default(),
			webhook_id: Default::default(),
			pinned: Default::default(),

			// TODO: support replies!
			reference: Default::default(),
			referenced_message: Default::default(),
		}
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectMessage {
	pub id: u64,
	pub channel_id: u64,
	pub author: User,

	pub timestamp_created: String,
	#[serde(default)]
	pub timestamp_edited: Option<String>,

	#[serde(default)]
	pub kind: MessageType,
	pub content: String,

	#[serde(default)]
	pub attachments: Vec<Attachment>,
	#[serde(default)]
	pub embeds: Vec<Embed>,
	#[serde(default)]
	pub reactions: Vec<MessageReaction>,

	#[serde(default)]
	pub application: Option<MessageApplication>,
	#[serde(default)]
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
			.header("accord-author-name", &escape(&self.author.name))
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

			kind: dis.kind.into(),
			content: dis.content.clone(),

			attachments: dis.attachments.clone(),
			embeds: dis.embeds.clone(),
			reactions: dis.reactions.clone(),

			application: dis.application.clone(),
			flags: dis.flags.map(MessageFlag::from_discord).unwrap_or_default(),
		}
	}
}

impl From<&DirectMessage> for DisMessage {
	/// Convert from a Raccord ServerMessage to a Discord Message
	fn from(rac: &DirectMessage) -> Self {
		Self {
			id: MessageId(rac.id),
			guild_id: None,
			channel_id: ChannelId(rac.channel_id),
			author: (&rac.author).into(),
			member: None,

			timestamp: rac.timestamp_created.clone(),
			edited_timestamp: rac.timestamp_edited.clone(),

			kind: rac.kind.into(),
			content: rac.content.clone(),

			attachments: rac.attachments.clone(),
			embeds: rac.embeds.clone(),
			reactions: rac.reactions.clone(),

			application: rac.application.clone(),
			flags: MessageFlag::to_discord(&rac.flags),

			activity: Default::default(),
			mention_channels: Default::default(),
			mention_everyone: Default::default(),
			mention_roles: Default::default(),
			mentions: Default::default(),
			stickers: Default::default(),
			tts: Default::default(),
			webhook_id: Default::default(),
			pinned: Default::default(),

			// TODO: support replies!
			reference: Default::default(),
			referenced_message: Default::default(),
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

fn escape(s: &str) -> String {
	s.escape_unicode().to_string()
}
