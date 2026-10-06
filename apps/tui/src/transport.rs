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
  fn archive_key(&self) -> String;
  /// Prevent connection prerequisites from reading or writing the host terminal
  /// while the TUI owns it. Startup authentication runs before this is enabled.
  fn set_terminal_ui_active(&self, _active: bool) {}
}

pub struct LocalTransport(pub PathBuf);

impl Transport for LocalTransport {
  fn connect(&self) -> ConnectFuture<'_> {
    Box::pin(
      async move { Ok(Box::new(ctmux_ipc::connect_or_start_daemon(&self.0).await?) as Stream) },
    )
  }

  fn archive_key(&self) -> String {
    self.0.to_string_lossy().into_owned()
  }
}
