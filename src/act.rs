use async_channel::Receiver;
use async_std::{prelude::StreamExt, task::spawn};
use serde::{de, Deserialize, Deserializer, Serialize};
use std::{convert::TryFrom, error::Error, fmt::Display, str::FromStr};
use twilight_http::{request::AuditLogReason, Client as HttpClient};
use twilight_model::id::{ChannelId, GuildId, RoleId, UserId};

use crate::error;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Act {
	CreateMessage {
		content: String,
		#[serde(default, deserialize_with = "maybe_str_opt")]
		channel_id: Option<u64>,
	},
	AssignRole {
		#[serde(deserialize_with = "maybe_str")]
		role_id: u64,
		#[serde(deserialize_with = "maybe_str")]
		user_id: u64,
		#[serde(default, deserialize_with = "maybe_str_opt")]
		server_id: Option<u64>,
		reason: Option<String>,
	},
	RemoveRole {
		#[serde(deserialize_with = "maybe_str")]
		role_id: u64,
		#[serde(deserialize_with = "maybe_str")]
		user_id: u64,
		#[serde(default, deserialize_with = "maybe_str_opt")]
		server_id: Option<u64>,
		reason: Option<String>,
	},
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MaybeStr {
	S(String),
	N(u64),
}

fn maybe_str<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
	T: FromStr + From<u64>,
	T::Err: Display,
	D: Deserializer<'de>,
{
	let s = MaybeStr::deserialize(deserializer)?;
	match s {
		MaybeStr::S(s) => T::from_str(&s).map_err(de::Error::custom),
		MaybeStr::N(n) => T::try_from(n).map_err(de::Error::custom),
	}
}

fn maybe_str_opt<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
	T: FromStr + From<u64>,
	T::Err: Display,
	D: Deserializer<'de>,
{
	let s = MaybeStr::deserialize(deserializer)?;
	Some(match s {
		MaybeStr::S(s) => T::from_str(&s).map_err(de::Error::custom),
		MaybeStr::N(n) => T::try_from(n).map_err(de::Error::custom),
	})
	.transpose()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Stage {
	pub act: Act,
	pub default_server_id: Option<GuildId>,
	pub default_channel_id: Option<ChannelId>,
}

pub async fn play_to_discord(
	http: HttpClient,
	mut feed: Receiver<Stage>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
	while let Some(stage) = feed.next().await {
		spawn(send_to_discord(http.clone(), stage));
	}

	Ok(())
}

async fn send_to_discord(
	http: HttpClient,
	stage: Stage,
) -> Result<(), Box<dyn Error + Send + Sync>> {
	let Stage {
		act,
		default_server_id,
		default_channel_id,
	} = stage;

	match act {
		Act::CreateMessage {
			content,
			channel_id,
		} => {
			let channel_id = channel_id
				.map(ChannelId)
				.or(default_channel_id)
				.ok_or(error::MissingChannel)?;
			http.create_message(channel_id).content(content)?.await?;
		}
		Act::AssignRole {
			role_id,
			user_id,
			server_id,
			reason,
		} => {
			let server_id = server_id
				.map(GuildId)
				.or(default_server_id)
				.ok_or(error::MissingServer)?;

			let mut add = http.add_guild_member_role(server_id, UserId(user_id), RoleId(role_id));

			if let Some(text) = reason {
				add = add.reason(text)?;
			}

			add.await?;
		}
		Act::RemoveRole {
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
				rm = rm.reason(text)?;
			}

			rm.await?;
		}
	}

	Ok(())
}
