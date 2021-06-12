use async_channel::{Receiver, Sender};
use async_std::{prelude::StreamExt, task::spawn};
use futures::io::{AsyncBufReadExt, AsyncRead, BufReader};
use isahc::{http::Response, ResponseExt};
use std::{error::Error, fmt::Debug, io::Read, str::FromStr, sync::Arc};
use tracing::{debug, error, info, trace, warn};
use twilight_cache_inmemory::{EventType, InMemoryCache};
use twilight_gateway::{cluster::Cluster, Event};
use twilight_http::Client as HttpClient;
use twilight_model::{
	gateway::{
		payload::update_status::UpdateStatusInfo,
		presence::{Activity, ActivityType, Status},
		Intents,
	},
	id::{ChannelId, GuildId, UserId},
};

use crate::{
	act::{Act, Stage},
	raccord,
};

pub struct Forward {
	pub cache: InMemoryCache,
	pub cluster: Cluster,
	pub http: HttpClient,
}

impl Forward {
	pub async fn init(
		token: String,
		target: Arc<raccord::Client>,
	) -> Result<Self, Box<dyn Error + Send + Sync>> {
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
						activities: presence.activity.map(|activity| {
							let (kind, name) = match activity {
								raccord::Activity::Playing { name } => {
									(ActivityType::Playing, name)
								}
								raccord::Activity::Streaming { name } => {
									(ActivityType::Streaming, name)
								}
								raccord::Activity::Listening { name } => {
									(ActivityType::Listening, name)
								}
								raccord::Activity::Watching { name } => {
									(ActivityType::Watching, name)
								}
								raccord::Activity::Custom { name } => (ActivityType::Custom, name),
							};

							vec![Activity {
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
							}]
						}),
					});
				}
			}
		}

		// TODO: env var control for intents (notably for privileged intents)
		let mut config = Cluster::builder(
			&token,
			Intents::DIRECT_MESSAGES | Intents::GUILD_MESSAGES | Intents::GUILD_MEMBERS,
		);

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

		Ok(Self {
			cache,
			cluster,
			http,
		})
	}

	pub async fn worker(
		self,
		target: Arc<raccord::Client>,
		ghosts: Receiver<(u64, Event)>,
		player: Sender<Stage>,
	) -> Result<(), Box<dyn Error + Send + Sync>> {
		let solids = self.cluster.events();
		let mut events = solids.merge(ghosts);

		while let Some((shard_id, event)) = events.next().await {
			spawn(handle_event(
				self.cache.clone(),
				target.clone(),
				shard_id,
				event,
				player.clone(),
			));
		}

		Ok(())
	}
}

pub async fn handle_event(
	cache: InMemoryCache,
	target: Arc<raccord::Client>,
	shard_id: u64,
	event: Event,
	player: Sender<Stage>,
) {
	if let Err(err) = try_event(cache, target, shard_id, event, player).await {
		error!("got error while handling event:\n{}", err);
	}
}

pub async fn try_event(
	cache: InMemoryCache,
	target: Arc<raccord::Client>,
	shard_id: u64,
	event: Event,
	player: Sender<Stage>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
	trace!("updating twilight cache");
	cache.update(&event);

	match event {
		Event::MessageCreate(message) if message.guild_id.is_some() => {
			debug!("received guild message create");
			let msg = raccord::ServerMessage::from(&**message);
			let res = if let Some(command) = target.parse_command(&msg.content) {
				trace!("submitting act: {:?}", command);
				target.post(raccord::Command {
					command,
					message: msg,
				})
			} else {
				trace!("submitting act: {:?}", msg);
				target.post(msg)
			}?
			.await?;
			trace!("handing off response: {:?}", res);
			handle_response(
				res,
				player,
				Some(message.guild_id.unwrap()),
				Some(message.channel_id),
				None,
			)
			.await?;
		}
		Event::MessageCreate(message) => {
			debug!("received direct message create");
			let msg = raccord::DirectMessage::from(&**message);
			let res = if let Some(command) = target.parse_command(&msg.content) {
				trace!("submitting act: {:?}", command);
				target.post(raccord::Command {
					command,
					message: msg,
				})
			} else {
				trace!("submitting act: {:?}", msg);
				target.post(msg)
			}?
			.await?;
			trace!("handing off response: {:?}", res);
			handle_response(res, player, None, Some(message.channel_id), None).await?;
		}
		Event::MemberAdd(mem) => {
			debug!("received guild member join");
			let member = raccord::Member::from(&**mem);
			trace!("submitting act: {:?}", member);
			let res = target.post(raccord::ServerJoin(member))?.await?;
			trace!("handing off response: {:?}", res);
			handle_response(res, player, Some(mem.guild_id), None, None).await?;
		}
		Event::ShardConnected(_) => {
			info!("connected on shard {}", shard_id);
			let res = target.post(raccord::Connected { shard: shard_id })?.await?;
			trace!("handing off response: {:?}", res);
			handle_response(res, player, None, None, None).await?;
		}
		_ => {}
	}

	Ok(())
}

async fn handle_response<T: Debug + Read + AsyncRead + Unpin>(
	mut res: Response<T>,
	player: Sender<Stage>,
	from_server: Option<GuildId>,
	from_channel: Option<ChannelId>,
	_from_user: Option<UserId>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
	let status = res.status();
	if status.is_informational() {
		warn!("unhandled information code {:?}", status);
	}

	if status == 204 || status == 404 {
		// no content, no action
		trace!("no action response: {:?}", status);
		return Ok(());
	}

	if status.is_redirection() {
		match status.into() {
			300 => warn!("TODO: multiple choice (http 300) is not designed yet"),
			301 | 302 | 303 | 307 | 308 => unreachable!("redirects should be handled by curl"),
			304 => error!("http 304 response caching not implemented"),
			305 | 306 => error!("proxy redirections (http 305 and 306) unsupported"),
			_ => error!("invalid 3xx code"),
		}

		return Ok(());
	}

	if status.is_client_error() || status.is_server_error() {
		error!(
			"error {:?} from target, TODO: more error handling here",
			status
		);
		return Ok(());
	}

	if !status.is_success() {
		error!("invalid response status: {:?}", status);
		return Ok(());
	}

	let content_type = res
		.headers()
		.get("content-type")
		.and_then(|s| s.to_str().ok())
		.and_then(|s| mime::Mime::from_str(s).ok())
		.unwrap_or(mime::APPLICATION_OCTET_STREAM);
	trace!("receiving content of type: {:?}", content_type);

	match (content_type.type_(), content_type.subtype()) {
		(mime::APPLICATION, mime::JSON) => {
			let default_server_id = res
				.headers()
				.get("accord-server-id")
				.and_then(|h| h.to_str().ok())
				.and_then(|s| u64::from_str(s).ok())
				.map(GuildId)
				.or(from_server);
			let default_channel_id = res
				.headers()
				.get("accord-channel-id")
				.and_then(|h| h.to_str().ok())
				.and_then(|s| u64::from_str(s).ok())
				.map(ChannelId)
				.or(from_channel);
			let has_content_length = res
				.headers()
				.get("content-length")
				.and_then(|s| s.to_str().ok())
				.and_then(|s| usize::from_str(s).ok())
				.unwrap_or(0) > 0;

			if has_content_length {
				info!("response has content-length, parsing single act");
				let act: Act = res.json()?;
				trace!("parsed act: {:?}", &act);
				player
					.send(Stage {
						act,
						default_server_id,
						default_channel_id,
					})
					.await?;
			} else {
				info!("response has no content-length, streaming multiple acts");

				let mut lines = BufReader::new(res.into_body()).lines();
				while let Some(line) = lines.next().await {
					let line = line?;
					trace!("got line: {:?}", line);
					let act: Act = serde_json::from_str(line.trim())?;
					trace!("parsed act: {:?}", &act);
					player
						.send(Stage {
							act,
							default_server_id,
							default_channel_id,
						})
						.await?;
				}

				info!("done streaming");
			}
		}
		(mime::TEXT, mime::PLAIN) => {
			let content = res.text()?;
			let header_channel = res
				.headers()
				.get("accord-channel-id")
				.and_then(|h| h.to_str().ok())
				.and_then(|s| u64::from_str(s).ok());

			player
				.send(Stage {
					act: Act::CreateMessage {
						content,
						channel_id: header_channel,
					},
					default_server_id: from_server,
					default_channel_id: from_channel,
				})
				.await?;
		}
		(t, s) => warn!("unhandled content-type {}/{}", t, s),
	}

	Ok(())
}
