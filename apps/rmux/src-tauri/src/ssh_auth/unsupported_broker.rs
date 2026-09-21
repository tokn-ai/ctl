//! Explicit errors for platforms without the Unix control-master broker.

use std::future::{Ready, ready};

use crate::dto::{ConnectionTargetDto, SshConnectionStatusDto};
use crate::error::{CommandErrorDto, CommandResult};

pub(super) fn unsupported<T>() -> Ready<CommandResult<T>> {
  ready(Err(CommandErrorDto::new(
    "ssh_broker_unsupported",
    "SSH connection status, disconnection, and port forwarding require macOS or Linux.",
  )))
}

pub(super) fn connection_status(
  _target: &ConnectionTargetDto,
) -> Ready<CommandResult<SshConnectionStatusDto>> {
  unsupported()
}

pub(super) fn configure_port_forward(
  _target: &ConnectionTargetDto,
  _forward: ctld_ipc::LocalPortForward,
  _enabled: bool,
) -> Ready<CommandResult<ctld_ipc::PortForwardStatus>> {
  unsupported()
}

pub(super) fn list_port_forwards(
  _target: &ConnectionTargetDto,
) -> Ready<CommandResult<Vec<ctld_ipc::PortForwardStatus>>> {
  unsupported()
}

pub(super) fn list_remote_listeners(
  _target: &ConnectionTargetDto,
) -> Ready<CommandResult<ctl_proto::TcpListenerCatalog>> {
  unsupported()
}
