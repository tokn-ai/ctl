//! Reconnect policy includes the VPN and broker prerequisites of an attachment.

use ctl_core::connection::is_transient_io_error;
use ctl_ipc::{CodecError, ConnectError};

use super::CtlConnectError;

pub(super) fn is_retryable(error: &CtlConnectError) -> bool {
  match error {
    CtlConnectError::Core(error) | CtlConnectError::Repair(crate::remote::Error::Core(error)) => {
      ctl_client::is_retryable_connection_error(error)
    }
    CtlConnectError::Target(error) => target_is_retryable(error),
    #[cfg(unix)]
    CtlConnectError::Broker(error) => broker_is_retryable(error),
    _ => false,
  }
}

fn target_is_retryable(error: &crate::target::Error) -> bool {
  match error {
    crate::target::Error::Connect(error) => connect_is_retryable(error),
    crate::target::Error::Vpn(error) => local_vpn_is_retryable(error),
    crate::target::Error::Runtime(error) => vpn_is_retryable(error),
    #[cfg(unix)]
    crate::target::Error::Broker(error) => broker_is_retryable(error),
    _ => false,
  }
}

fn vpn_is_retryable(error: &crate::vpn::Error) -> bool {
  match error {
    crate::vpn::Error::Remote(error) => error.is_retryable_connection(),
    crate::vpn::Error::Preparation(error) => target_is_retryable(error),
    crate::vpn::Error::Vpn(error) => local_vpn_is_retryable(error),
    #[cfg(unix)]
    crate::vpn::Error::Broker(error) => broker_is_retryable(error),
    _ => false,
  }
}

fn local_vpn_is_retryable(error: &ctl_ipc::vpn::VpnError) -> bool {
  match error {
    ctl_ipc::vpn::VpnError::Connect(error) => connect_is_retryable(error),
    ctl_ipc::vpn::VpnError::Codec(error) => codec_is_retryable(error),
    ctl_ipc::vpn::VpnError::Timeout => true,
    _ => false,
  }
}

#[cfg(unix)]
fn broker_is_retryable(error: &crate::ssh_broker::Error) -> bool {
  match error {
    crate::ssh_broker::Error::Connect(error) => connect_is_retryable(error),
    crate::ssh_broker::Error::Codec(error) => codec_is_retryable(error),
    crate::ssh_broker::Error::ConnectionClosed | crate::ssh_broker::Error::StatusTimeout => true,
    crate::ssh_broker::Error::Daemon { code, .. } => {
      matches!(code.as_str(), "ssh_timeout" | "ssh_connection_failed")
    }
    _ => false,
  }
}

fn connect_is_retryable(error: &ConnectError) -> bool {
  match error {
    ConnectError::Connect(error) => {
      error.kind() == std::io::ErrorKind::NotFound || is_transient_io_error(error)
    }
    _ => false,
  }
}

fn codec_is_retryable(error: &CodecError) -> bool {
  matches!(error, CodecError::Io(error) if is_transient_io_error(error))
}

#[cfg(test)]
mod tests;
