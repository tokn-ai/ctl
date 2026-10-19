//! Dedicated forwarding works without a Unix control-master broker.

use std::future::{Ready, ready};

use crate::dto::{ConnectionTargetDto, SshConnectionStatusDto};
use crate::error::{CommandErrorDto, CommandResult};

pub(super) fn unsupported<T>() -> Ready<CommandResult<T>> {
  ready(Err(CommandErrorDto::new(
    "ssh_broker_unsupported",
    "SSH connection status, disconnection, and remote listener discovery require macOS or Linux.",
  )))
}

pub(super) fn connection_status(
  _target: &ConnectionTargetDto,
) -> Ready<CommandResult<SshConnectionStatusDto>> {
  unsupported()
}

pub(super) use super::forwarding::{configure_port_forward, list_port_forwards};

pub(super) fn list_remote_listeners(
  _target: &ConnectionTargetDto,
) -> Ready<CommandResult<ctl_proto::TcpListenerCatalog>> {
  unsupported()
}
