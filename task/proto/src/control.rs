//! Versioned daemon lifecycle requests, independent of the task protocol.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
  RestartDaemon { protocol_version: u16 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
  RestartAccepted {
    data_directory: PathBuf,
    rmux_socket: PathBuf,
  },
  Error {
    message: String,
  },
}
