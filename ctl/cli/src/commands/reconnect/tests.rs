use std::io::{self, ErrorKind};

use ctmux_cli::Connector as _;

use super::*;
use crate::commands::CtlConnector;

fn retryable(error: &CtlConnectError) -> bool {
  let connector = CtlConnector {
    target: ctl_client::ConnectionTarget::ssh("work"),
    settings: ctl_client::hosts::ConnectionTargetDto::ssh("work"),
    recovery: std::sync::Arc::default(),
    terminal_ui_active: std::sync::Arc::default(),
    #[cfg(unix)]
    broker: std::sync::Arc::default(),
  };
  connector.is_retryable(error)
}

fn remote_vpn(error: ctl_ipc::remote_vpn::Error) -> CtlConnectError {
  CtlConnectError::Target(crate::target::Error::Runtime(crate::vpn::Error::Remote(
    error,
  )))
}

#[test]
fn remote_vpn_prerequisite_interruptions_keep_the_attachment_reconnect_loop_alive() {
  for kind in [
    ErrorKind::UnexpectedEof,
    ErrorKind::ConnectionReset,
    ErrorKind::BrokenPipe,
    ErrorKind::TimedOut,
  ] {
    assert!(retryable(&remote_vpn(ctl_ipc::remote_vpn::Error::Io(
      io::Error::from(kind),
    ))));
  }
  assert!(retryable(&remote_vpn(ctl_ipc::remote_vpn::Error::Timeout)));
  assert!(retryable(&remote_vpn(
    ctl_ipc::remote_vpn::Error::ConnectionClosed("protocol negotiation"),
  )));
  assert!(retryable(&remote_vpn(ctl_ipc::remote_vpn::Error::Codec(
    CodecError::Io(ErrorKind::UnexpectedEof.into()),
  ))));
  assert!(retryable(&CtlConnectError::Target(
    crate::target::Error::Runtime(crate::vpn::Error::Preparation(Box::new(
      crate::target::Error::Runtime(crate::vpn::Error::Remote(
        ctl_ipc::remote_vpn::Error::Timeout,
      )),
    ))),
  )));
}

#[test]
fn remote_vpn_security_and_protocol_failures_stop_reconnecting() {
  for error in [
    ctl_ipc::remote_vpn::Error::IdentityMismatch,
    ctl_ipc::remote_vpn::Error::UnsupportedAgent,
    ctl_ipc::remote_vpn::Error::UnsupportedProtocol,
    ctl_ipc::remote_vpn::Error::InvalidRequest("invalid destination".into()),
    ctl_ipc::remote_vpn::Error::UnexpectedResponse,
    ctl_ipc::remote_vpn::Error::Io(ErrorKind::PermissionDenied.into()),
    ctl_ipc::remote_vpn::Error::Io(ErrorKind::InvalidData.into()),
  ] {
    assert!(!retryable(&remote_vpn(error)));
  }
  for error in [
    crate::target::Error::MissingVpn,
    crate::target::Error::VpnUnavailable,
    crate::target::Error::Profile(crate::vpn::profiles::Error::Invalid),
  ] {
    assert!(!retryable(&CtlConnectError::Target(error)));
  }
}

#[test]
fn initial_repair_wrapper_preserves_transport_retry_classification() {
  for kind in [ErrorKind::TimedOut, ErrorKind::ConnectionReset] {
    assert!(retryable(&CtlConnectError::Repair(
      crate::remote::Error::Core(ctl_client::CoreError::RemoteIdentity(kind.into())),
    )));
  }
  assert!(!retryable(&CtlConnectError::Repair(
    crate::remote::Error::Core(ctl_client::CoreError::RemoteIdentity(
      ErrorKind::InvalidData.into(),
    )),
  )));
  assert!(!retryable(&CtlConnectError::Repair(
    crate::remote::Error::Cancelled,
  )));
}

#[cfg(unix)]
#[test]
fn broker_restart_and_transport_failures_can_recover_without_hiding_authentication_errors() {
  for kind in [ErrorKind::NotFound, ErrorKind::ConnectionRefused] {
    assert!(retryable(&CtlConnectError::Broker(
      crate::ssh_broker::Error::Connect(ConnectError::Connect(kind.into())),
    )));
  }
  assert!(retryable(&CtlConnectError::Target(
    crate::target::Error::Broker(crate::ssh_broker::Error::Codec(CodecError::Io(
      ErrorKind::BrokenPipe.into(),
    ))),
  )));
  assert!(retryable(&CtlConnectError::Target(
    crate::target::Error::Runtime(crate::vpn::Error::Broker(
      crate::ssh_broker::Error::ConnectionClosed,
    )),
  )));
  for code in ["ssh_timeout", "ssh_connection_failed"] {
    assert!(retryable(&CtlConnectError::Broker(
      crate::ssh_broker::Error::Daemon {
        code: code.into(),
        message: "Network unavailable during connection".into(),
      },
    )));
  }
  for code in [
    "ssh_authentication_failed",
    "ssh_host_disconnected",
    "ssh_config_error",
    "ctld_connection_error",
    "ctld_protocol_version_mismatch",
  ] {
    assert!(!retryable(&CtlConnectError::Broker(
      crate::ssh_broker::Error::Daemon {
        code: code.into(),
        message: "The connection needs user action".into(),
      },
    )));
  }
  assert!(!retryable(&CtlConnectError::Broker(
    crate::ssh_broker::Error::AuthenticationRequired,
  )));
  assert!(!retryable(&CtlConnectError::Broker(
    crate::ssh_broker::Error::Codec(CodecError::Io(ErrorKind::PermissionDenied.into())),
  )));
}

#[test]
fn local_vpn_transport_interruptions_follow_the_same_reconnect_policy() {
  assert!(retryable(&CtlConnectError::Target(
    crate::target::Error::Vpn(ctl_ipc::vpn::VpnError::Codec(CodecError::Io(
      ErrorKind::UnexpectedEof.into(),
    ))),
  )));
  assert!(!retryable(&CtlConnectError::Target(
    crate::target::Error::Vpn(ctl_ipc::vpn::VpnError::InvalidConnection(
      "Invalid saved VPN".into(),
    )),
  )));
}
