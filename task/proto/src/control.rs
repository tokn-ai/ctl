//! Versioned daemon lifecycle requests, independent of the task protocol.
use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Internal protocol build; incrementing this does not publish a new contract.
pub const PROTOCOL_BUILD: u16 = 2;
/// First published wire contract. Keep this identity immutable.
pub const CONTRACT_V1_0_2: ProtocolVersion = ProtocolVersion::new(1, 0, 2);
pub const PROTOCOL_VERSION: ProtocolVersion = CONTRACT_V1_0_2;
pub const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[CONTRACT_V1_0_2];

#[must_use]
pub fn protocol_offer() -> ProtocolOffer {
  ProtocolOffer::new(
    PROTOCOL_BUILD,
    PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
  )
}

#[must_use]
pub fn protocol_info() -> ctl_core::component::ProtocolInfo {
  ctl_core::component::ProtocolInfo::new(
    "task_control",
    PROTOCOL_BUILD,
    PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
  )
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
  RestartDaemon { protocol: ProtocolOffer },
  ComponentStatus { protocol: ProtocolOffer },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
  ComponentStatus {
    build: ctl_core::component::ComponentBuildInfo,
    protocol_version: ProtocolVersion,
    data_protocol_version: ProtocolVersion,
    protocols: Vec<ctl_core::component::ProtocolInfo>,
  },
  RestartAccepted {
    protocol_version: ProtocolVersion,
    data_directory: PathBuf,
    ctmux_socket: PathBuf,
  },
  Error {
    message: String,
  },
}
