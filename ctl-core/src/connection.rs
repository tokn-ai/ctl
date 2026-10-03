//! Connection failures that may recover without changing configuration.

use std::io;

/// Whether reopening a connection can recover from this I/O failure.
/// Missing executables, invalid data, and permission failures need intervention.
#[must_use]
pub fn is_transient_io_error(error: &io::Error) -> bool {
  matches!(
    error.kind(),
    io::ErrorKind::ConnectionRefused
      | io::ErrorKind::ConnectionReset
      | io::ErrorKind::ConnectionAborted
      | io::ErrorKind::NotConnected
      | io::ErrorKind::BrokenPipe
      | io::ErrorKind::TimedOut
      | io::ErrorKind::Interrupted
      | io::ErrorKind::UnexpectedEof
      | io::ErrorKind::WouldBlock
      | io::ErrorKind::HostUnreachable
      | io::ErrorKind::NetworkUnreachable
      | io::ErrorKind::NetworkDown
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn network_interruptions_can_recover_but_configuration_failures_cannot() {
    for kind in [
      io::ErrorKind::ConnectionRefused,
      io::ErrorKind::ConnectionReset,
      io::ErrorKind::ConnectionAborted,
      io::ErrorKind::NotConnected,
      io::ErrorKind::BrokenPipe,
      io::ErrorKind::TimedOut,
      io::ErrorKind::Interrupted,
      io::ErrorKind::UnexpectedEof,
      io::ErrorKind::WouldBlock,
      io::ErrorKind::HostUnreachable,
      io::ErrorKind::NetworkUnreachable,
      io::ErrorKind::NetworkDown,
    ] {
      assert!(is_transient_io_error(&kind.into()), "{kind:?}");
    }
    for kind in [
      io::ErrorKind::NotFound,
      io::ErrorKind::InvalidData,
      io::ErrorKind::InvalidInput,
      io::ErrorKind::PermissionDenied,
      io::ErrorKind::Unsupported,
      io::ErrorKind::Other,
    ] {
      assert!(!is_transient_io_error(&kind.into()), "{kind:?}");
    }
  }
}
