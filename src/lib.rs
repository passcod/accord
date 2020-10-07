use async_std::prelude::StreamExt;
use futures::io::{AsyncBufReadExt, AsyncRead, BufReader};
use isahc::{http::Response, ResponseExt};
use std::{error::Error, fmt::Debug, io::Read, str::FromStr, sync::Arc};
use tracing::{error, info, trace, warn};
use twilight_gateway::Event;
use twilight_http::Client as HttpClient;
use twilight_model::id::{ChannelId, GuildId, RoleId, UserId};

pub mod error;
pub mod raccord;

pub async fn handle_response<T: Debug + Read + AsyncRead + Unpin>(
    mut res: Response<T>,
    http: HttpClient,
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
                .unwrap_or(0)
                > 0;

            if has_content_length {
                info!("response has content-length, parsing single act");
                let act: raccord::Act = res.json()?;
                handle_act(http, act, default_server_id, default_channel_id).await?;
            } else {
                info!("response has no content-length, streaming multiple acts");
                let mut lines = BufReader::new(res.into_body()).lines();
                loop {
                    if let Some(line) = lines.next().await {
                        let line = line?;
                        let act: raccord::Act = serde_json::from_str(line.trim())?;
                        handle_act(http.clone(), act, default_server_id, default_channel_id)
                            .await?;
                    } else {
                        break;
                    }
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

            handle_act(
                http,
                raccord::Act::CreateMessage {
                    content,
                    channel_id: header_channel,
                },
                from_server,
                from_channel,
            )
            .await?;
        }
        (t, s) => warn!("unhandled content-type {}/{}", t, s),
    }

    Ok(())
}

pub async fn handle_act(
    http: HttpClient,
    act: raccord::Act,
    default_server_id: Option<GuildId>,
    default_channel_id: Option<ChannelId>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match act {
        raccord::Act::CreateMessage {
            content,
            channel_id,
        } => {
            let channel_id = channel_id
                .map(ChannelId)
                .or(default_channel_id)
                .ok_or(error::MissingChannel)?;
            http.create_message(channel_id).content(content)?.await?;
        }
        raccord::Act::AssignRole {
            role_id,
            user_id,
            server_id,
            reason,
        } => {
            let server_id = server_id
                .map(GuildId)
                .or(default_server_id)
                .ok_or(error::MissingServer)?;

            let mut add = http.add_role(server_id, UserId(user_id), RoleId(role_id));

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
            let server_id = server_id
                .map(GuildId)
                .or(default_server_id)
                .ok_or(error::MissingServer)?;

            let mut rm = http.remove_guild_member_role(server_id, UserId(user_id), RoleId(role_id));

            if let Some(text) = reason {
                rm = rm.reason(text);
            }

            rm.await?;
        }
    }

    Ok(())
}

pub async fn handle_event(
    target: Arc<raccord::Client>,
    shard_id: u64,
    event: Event,
    http: HttpClient,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match event {
        Event::MessageCreate(message) if message.guild_id.is_some() => {
            let msg = raccord::ServerMessage::from(&**message);
            let res = if let Some(command) = target.parse_command(&msg.content) {
                target.post(raccord::Command {
                    command,
                    message: msg,
                })
            } else {
                target.post(msg)
            }?
            .await?;
            handle_response(
                res,
                http,
                Some(message.guild_id.unwrap()),
                Some(message.channel_id),
                None,
            )
            .await?;
        }
        Event::MessageCreate(message) => {
            let msg = raccord::DirectMessage::from(&**message);
            let res = if let Some(command) = target.parse_command(&msg.content) {
                target.post(raccord::Command {
                    command,
                    message: msg,
                })
            } else {
                target.post(msg)
            }?
            .await?;
            handle_response(res, http, None, Some(message.channel_id), None).await?;
        }
        Event::MemberAdd(mem) => {
            let member = raccord::Member::from(&**mem);
            let res = target.post(raccord::ServerJoin(member))?.await?;
            handle_response(res, http, Some(mem.guild_id), None, None).await?;
        }
        Event::ShardConnected(_) => {
            info!("connected on shard {}", shard_id);
            let res = target.post(raccord::Connected { shard: shard_id })?.await?;
            handle_response(res, http, None, None, None).await?;
        }
        _ => {}
    }

    Ok(())
}
