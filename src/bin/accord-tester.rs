use accord::{
	act::Stage,
	forward,
	raccord::{Client, Sendable},
	reverse,
};
use async_channel::{unbounded, Receiver, Sender};
use async_std::{
	prelude::{FutureExt, StreamExt},
	task::spawn,
};
use serde::Serialize;
use std::{env, error::Error, sync::Arc};
use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter, FmtSubscriber};
use twilight_cache_inmemory::InMemoryCache;
use twilight_gateway::Event;

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
	tracing_log::LogTracer::init()?;
	let rustlog = env::var("RUST_LOG").unwrap_or(String::from("info,accord=trace"));
	if rustlog.split(',').any(|p| p == "pretty") {
		let subscriber = FmtSubscriber::builder()
			.pretty()
			.with_span_events(FmtSpan::CLOSE)
			.with_env_filter(EnvFilter::new(rustlog))
			.finish();
		tracing::subscriber::set_global_default(subscriber)?;
	} else {
		let subscriber = FmtSubscriber::builder()
			.with_span_events(FmtSpan::CLOSE)
			.with_env_filter(EnvFilter::new(rustlog))
			.finish();
		tracing::subscriber::set_global_default(subscriber)?;
	};

	let bind = env::var("ACCORD_BIND").unwrap_or_else(|_| String::from("localhost:8181"));
	let target_base = env::var("ACCORD_TARGET").expect("FATAL: missing env: ACCORD_TARGET");
	let command_match = env::var("ACCORD_COMMAND_MATCH").ok();
	let command_parse = env::var("ACCORD_COMMAND_PARSE").ok();
	let target = Arc::new(Client::new(target_base, command_match, command_parse));

	let (act_s, act_r) = unbounded();
	let (ghost_s, ghost_r) = unbounded();

	// TODO: sort out that match into something a little less of a mess
	match play_to_target(target.clone(), act_r)
		.join(reverse::server(bind, ghost_s))
		.join(false_forward(target, ghost_r, act_s))
		.await
	{
		((Ok(_), Ok(_)), Ok(_)) => Ok(()),
		((Ok(_), Ok(_)), Err(e)) | ((Ok(_), Err(e)), Ok(_)) | ((Err(e), Ok(_)), Ok(_)) => Err(e),
		((Ok(_), Err(f)), Err(g)) | ((Err(f), Ok(_)), Err(g)) | ((Err(f), Err(g)), Ok(_)) => {
			eprintln!("{}", f);
			Err(g)
		}
		((Err(e), Err(f)), Err(g)) => {
			eprintln!("{}", e);
			eprintln!("{}", f);
			Err(g)
		}
	}
}

async fn false_forward(
	target: Arc<Client>,
	mut events: Receiver<(u64, Event)>,
	player: Sender<Stage>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
	let cache = InMemoryCache::builder().build();

	while let Some((shard_id, event)) = events.next().await {
		spawn(forward::handle_event(
			cache.clone(),
			target.clone(),
			shard_id,
			event,
			player.clone(),
		));
	}

	Ok(())
}

async fn play_to_target(
	target: Arc<Client>,
	mut feed: Receiver<Stage>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
	while let Some(stage) = feed.next().await {
		target.post(Wrap(stage))?.await?;
	}

	Ok(())
}

#[derive(Clone, Debug, Serialize)]
struct Wrap(Stage);

impl Sendable for Wrap {
	fn url(&self) -> String {
		String::from("/test/act")
	}
}
