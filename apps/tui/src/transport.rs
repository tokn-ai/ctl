use crate::Result;
use std::{future::Future, path::PathBuf, pin::Pin};
use tokio::io::{AsyncRead, AsyncWrite};

/// A duplex ctmux connection, including SSH-backed streams.
pub trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

/// A freshly connected protocol stream.
pub type Stream = Box<dyn Duplex>;
/// A connection attempt borrowing its transport settings.
pub type ConnectFuture<'a> = Pin<Box<dyn Future<Output = Result<Stream>> + Send + 'a>>;

/// Opens connections for requests, panes, and reconnects in the shared TUI.
pub trait Transport: Sync {
  fn connect(&self) -> ConnectFuture<'_>;
  /// Optional explicit reconnect preparation. The caller must release terminal
  /// ownership and stop its input reader before polling this authentication job.
  fn reconnect_interactive(&self) -> Option<ConnectFuture<'_>> {
    None
  }
  fn archive_key(&self) -> String;
  /// Human-readable selected host or endpoint, shown in session management.
  fn label(&self) -> &str;
  /// Remote session creation leaves cwd unset so the remote shell uses its home.
  fn is_local(&self) -> bool {
    true
  }
  /// Authentication and configuration failures suspend automatic retries.
  /// Transports without a classification retain the existing retry behavior.
  fn is_retryable(&self, _error: &crate::Error) -> bool {
    true
  }
  /// Missing host authorization can resume after a broker recovery event.
  /// Other permanent failures continue to require explicit user action.
  fn is_authentication_required(&self, _error: &crate::Error) -> bool {
    false
  }
  fn recovery_generation(&self) -> u64 {
    0
  }
  /// Prevent connection prerequisites from reading or writing the host terminal
  /// while the TUI owns it. Startup authentication runs before this is enabled.
  fn set_terminal_ui_active(&self, _active: bool) {}
}

pub struct LocalTransport(pub PathBuf);

impl Transport for LocalTransport {
  fn label(&self) -> &'static str {
    "local"
  }

  fn connect(&self) -> ConnectFuture<'_> {
    Box::pin(
      async move { Ok(Box::new(ctmux_ipc::connect_or_start_daemon(&self.0).await?) as Stream) },
    )
  }

  fn archive_key(&self) -> String {
    self.0.to_string_lossy().into_owned()
  }
}
