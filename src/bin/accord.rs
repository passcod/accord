use accord::{forward, raccord, reverse};
use async_channel::unbounded;
use async_std::prelude::FutureExt;
use std::{env, error::Error, sync::Arc};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

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

    match forward::worker(token, target, r)
        .join(reverse::server(bind, s))
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
