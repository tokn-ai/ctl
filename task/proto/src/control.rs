//! Versioned daemon lifecycle requests, independent of the task protocol.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PROTOCOL_VERSION: u16 = 2;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
  RestartDaemon { protocol_version: u16 },
  ComponentStatus { protocol_version: u16 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
  ComponentStatus {
    build: ctl_core::component::ComponentBuildInfo,
    protocol_version: u16,
  },
  RestartAccepted {
    data_directory: PathBuf,
    ctmux_socket: PathBuf,
  },
  Error {
    message: String,
  },
}
