use crate::raccord::{DirectMessage, ServerMessage};
use async_channel::Sender;
use std::{error::Error, fmt::Debug};
use tide::Server;
use tide::{Request, Response, StatusCode};
use tide_tracing::TraceMiddleware;
use twilight_gateway::Event;
use twilight_model::{channel::Message, gateway::payload::MessageCreate};

pub async fn server(
	bind: String,
	ghosts: Sender<(u64, Event)>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
	#[derive(Clone, Debug)]
	struct State {
		pub ghosts: Sender<(u64, Event)>,
	}

	let mut app = Server::with_state(State { ghosts });
	app.with(TraceMiddleware::new());

	app.at("/ghost/server/:server/channel/:channel/message")
		.post(|mut req: Request<State>| async move {
			// TODO: handle text/plain body
			let message: ServerMessage = req.body_json().await?;
			// TODO: log(warn) if :channel != message.channel etc
			let message: Message = (&message).into();
			let event = Event::MessageCreate(Box::new(MessageCreate(message)));
			req.state().ghosts.send((0, event)).await?;
			Ok(Response::new(StatusCode::NoContent))
		});

	app.at("/ghost/direct/channel/:channel/message")
		.post(|mut req: Request<State>| async move {
			// TODO: handle text/plain body
			let message: DirectMessage = req.body_json().await?;
			// TODO: log(warn) if :channel != message.channel
			let message: Message = (&message).into();
			let event = Event::MessageCreate(Box::new(MessageCreate(message)));
			req.state().ghosts.send((0, event)).await?;
			Ok(Response::new(StatusCode::NoContent))
		});

	app.listen(bind).await?;
	Ok(())
}
