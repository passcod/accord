use thiserror::Error;

#[derive(Copy, Clone, Debug, Error)]
#[error("no server information available")]
pub struct MissingServer;

#[derive(Copy, Clone, Debug, Error)]
#[error("no channel information available")]
pub struct MissingChannel;
