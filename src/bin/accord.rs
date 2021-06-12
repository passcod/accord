use accord::{act, raccord, reverse, Forward};
use async_channel::unbounded;
use async_std::prelude::FutureExt;
use std::{env, error::Error, sync::Arc};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
	tracing_log::LogTracer::init()?;
	let subscriber = FmtSubscriber::builder()
		.with_env_filter(EnvFilter::new(
			env::var("RUST_LOG").unwrap_or_else(|_| String::from("info")),
		))
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

	let fwd = Forward::init(token, target.clone()).await?;

	let (act_s, act_r) = unbounded();
	let (ghost_s, ghost_r) = unbounded();

	// TODO: sort out that match into something a little less of a mess
	match act::play_to_discord(fwd.http.clone(), act_r)
		.join(reverse::server(bind, ghost_s))
		.join(fwd.worker(target, ghost_r, act_s))
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
