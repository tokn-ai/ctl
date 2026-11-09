//! Bounded service streams sharing one authenticated SSH command channel.

use std::future::{Future, pending, poll_fn};
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, ReadBuf};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt as _, TokioAsyncReadCompatExt as _};

pub const PROTOCOL_BUILD: u16 = 1;
pub const CONTRACT_V1_0_1: ProtocolVersion = ProtocolVersion::new(1, 0, 1);
pub const PROTOCOL_VERSION: ProtocolVersion = CONTRACT_V1_0_1;
pub const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[CONTRACT_V1_0_1];
pub const PREFACE: &[u8] = b"ctl-service-transport\n";
pub const MAX_STREAMS: usize = 64;
pub const MAX_RECEIVE_BYTES: usize = 16 * 1024 * 1024;
const MAX_NEGOTIATION_BYTES: usize = 8192;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const MIN_STREAM_RECEIVE_BYTES: usize = 256 * 1024;

/// Only fixed services are selectable. Socket paths and commands stay remote-owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Service {
  Ctmux = 0,
  Task = 1,
  Udp = 2,
}

impl Service {
  fn decode(value: u8) -> io::Result<Self> {
    match value {
      0 => Ok(Self::Ctmux),
      1 => Ok(Self::Task),
      2 => Ok(Self::Udp),
      _ => Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Unknown shared transport service",
      )),
    }
  }
}

/// One independently flow-controlled logical service connection.
pub struct Stream {
  // Queue Yamux's stream cleanup before publishing free admission capacity.
  // Struct fields are dropped in declaration order.
  inner: Compat<yamux::Stream>,
  _lifetime: Option<ActiveStream>,
}

impl AsyncRead for Stream {
  fn poll_read(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buffer: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    Pin::new(&mut self.inner).poll_read(cx, buffer)
  }
}

impl AsyncWrite for Stream {
  fn poll_write(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    bytes: &[u8],
  ) -> Poll<io::Result<usize>> {
    Pin::new(&mut self.inner).poll_write(cx, bytes)
  }

  fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Pin::new(&mut self.inner).poll_flush(cx)
  }

  fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Pin::new(&mut self.inner).poll_shutdown(cx)
  }
}

struct ActiveStream(Arc<State>);

impl Drop for ActiveStream {
  fn drop(&mut self) {
    self.0.active.fetch_sub(1, Ordering::Release);
    self.0.activity.notify_one();
  }
}

// Yamux treats its own stream-limit error as connection-fatal. Admission must
// wait until queued stream drops have been processed, including when a pending
// physical write prevents its driver from draining the stream command queues.
struct ObservedIo<I> {
  inner: I,
  write_blocked: Arc<AtomicBool>,
  pause_reads_on_write: bool,
}

impl<I: AsyncRead + Unpin> AsyncRead for ObservedIo<I> {
  fn poll_read(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buffer: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    if self.pause_reads_on_write && self.write_blocked.load(Ordering::Relaxed) {
      // A rejected inbound stream must leave Yamux's stream map before the
      // next SYN is read. Pending writes already registered our driver waker.
      // Only the server gates reads; the client keeps draining physical input.
      return Poll::Pending;
    }
    Pin::new(&mut self.inner).poll_read(cx, buffer)
  }
}

impl<I: AsyncWrite + Unpin> AsyncWrite for ObservedIo<I> {
  fn poll_write(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    bytes: &[u8],
  ) -> Poll<io::Result<usize>> {
    let result = Pin::new(&mut self.inner).poll_write(cx, bytes);
    self
      .write_blocked
      .fetch_or(result.is_pending(), Ordering::Relaxed);
    result
  }

  fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    let result = Pin::new(&mut self.inner).poll_flush(cx);
    self
      .write_blocked
      .fetch_or(result.is_pending(), Ordering::Relaxed);
    result
  }

  fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Pin::new(&mut self.inner).poll_shutdown(cx)
  }
}

struct State {
  closed: watch::Sender<bool>,
  active: AtomicUsize,
  activity: Notify,
}

struct DriverState(Arc<State>);

impl Drop for DriverState {
  fn drop(&mut self) {
    self.0.closed.send_replace(true);
  }
}

struct OpenRequest {
  response: oneshot::Sender<io::Result<Stream>>,
}

/// A clonable handle to one shared physical transport.
#[derive(Clone)]
pub struct Client {
  requests: mpsc::Sender<OpenRequest>,
  state: Arc<State>,
}

impl Client {
  /// Opens a service without opening another SSH command session.
  ///
  /// # Errors
  /// Returns closure, capacity, negotiation, or timeout errors.
  pub async fn open(&self, service: Service) -> io::Result<Stream> {
    timeout(STARTUP_TIMEOUT, async {
      if self.is_closed() {
        return Err(closed());
      }
      let (response, received) = oneshot::channel();
      self
        .requests
        .send(OpenRequest { response })
        .await
        .map_err(|_| closed())?;
      let mut stream = received.await.map_err(|_| closed())??;
      stream.write_u8(service as u8).await?;
      stream.flush().await?;
      Ok(stream)
    })
    .await
    .map_err(|_| {
      io::Error::new(
        io::ErrorKind::TimedOut,
        "Opening a shared service stream timed out",
      )
    })?
  }

  /// Reports physical failure, explicit closure, or an expired idle connection.
  #[must_use]
  pub fn is_closed(&self) -> bool {
    *self.state.closed.borrow()
  }

  /// Closes this physical transport and every logical service stream.
  pub fn close(&self) {
    self.state.closed.send_replace(true);
  }

  /// Waits until the physical transport becomes unavailable.
  pub async fn closed(&self) {
    let mut receiver = self.state.closed.subscribe();
    while !*receiver.borrow_and_update() {
      if receiver.changed().await.is_err() {
        return;
      }
    }
  }
}

/// Negotiates and drives one client-side shared transport.
///
/// # Errors
/// Rejects unavailable, incompatible, oversized, or stalled protocol offers.
pub async fn connect<I>(mut io: I) -> io::Result<Client>
where
  I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
  timeout(STARTUP_TIMEOUT, async {
    let mut marker = [0; PREFACE.len()];
    io.read_exact(&mut marker).await?;
    if marker != PREFACE {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Remote agent does not support shared service streams",
      ));
    }
    let offer: ProtocolOffer = read_message(&mut io).await?;
    let selected = offer
      .negotiate(SUPPORTED_PROTOCOL_VERSIONS)
      .ok_or_else(incompatible)?;
    write_message(
      &mut io,
      &Selection {
        protocol_version: selected,
      },
    )
    .await
  })
  .await
  .map_err(|_| {
    io::Error::new(
      io::ErrorKind::TimedOut,
      "Shared transport negotiation timed out",
    )
  })??;
  let (requests, receiver) = mpsc::channel(32);
  let (closed, _) = watch::channel(false);
  let state = Arc::new(State {
    closed,
    active: AtomicUsize::new(0),
    activity: Notify::new(),
  });
  let driver_state = Arc::clone(&state);
  let closed_on_exit = DriverState(Arc::clone(&state));
  tokio::spawn(async move {
    // Construct the guard before spawning so cancellation also marks a driver
    // that the runtime drops before its first poll as closed.
    let _closed_on_exit = closed_on_exit;
    drive_client(io, receiver, driver_state).await;
  });
  Ok(Client { requests, state })
}

async fn drive_client<I>(io: I, mut requests: mpsc::Receiver<OpenRequest>, state: Arc<State>)
where
  I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
  let write_blocked = Arc::new(AtomicBool::new(false));
  let io = ObservedIo {
    inner: io,
    write_blocked: Arc::clone(&write_blocked),
    pause_reads_on_write: false,
  };
  let mut connection =
    yamux::Connection::new(io.compat(), config(MAX_STREAMS), yamux::Mode::Client);
  let mut cancellation = state.closed.subscribe();
  let mut requests_closed = false;
  let mut idle_since = Some(Instant::now());
  loop {
    if *cancellation.borrow() {
      break;
    }
    if state.active.load(Ordering::Acquire) == 0 {
      if requests_closed {
        break;
      }
      idle_since.get_or_insert_with(Instant::now);
    } else {
      idle_since = None;
    }
    tokio::select! {
      biased;
      _ = cancellation.changed() => break,
      request = requests.recv(), if !requests_closed => {
        let Some(mut request) = request else { requests_closed = true; continue; };
        if request.response.is_closed() { continue; }
        let opened = tokio::select! {
          _ = cancellation.changed() => Err(closed()),
          () = request.response.closed() => continue,
          result = poll_fn(|cx| {
            let at_capacity = state.active.load(Ordering::Acquire) >= MAX_STREAMS;
            write_blocked.store(false, Ordering::Relaxed);
            // Outbound creation also needs to drive incoming ACK/window frames.
            if connection.poll_next_inbound(cx).is_ready() {
              state.closed.send_replace(true);
              return Poll::Ready(Err(closed()));
            }
            if at_capacity {
              return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("The shared SSH service transport has reached its limit of {MAX_STREAMS} open streams. Close an unused service connection, then try again."),
              )));
            }
            if write_blocked.load(Ordering::Relaxed) {
              return Poll::Pending;
            }
            connection.poll_new_outbound(cx).map_err(io::Error::other)
          }) => result,
        };
        match opened {
          Ok(stream) => {
            state.active.fetch_add(1, Ordering::Relaxed);
            let stream = Stream { inner: stream.compat(), _lifetime: Some(ActiveStream(Arc::clone(&state))) };
            let _ = request.response.send(Ok(stream));
          },
          Err(error) => { let _ = request.response.send(Err(error)); },
        }
      },
      () = state.activity.notified() => {},
      _ = poll_fn(|cx| connection.poll_next_inbound(cx)) => break,
      () = async {
        if let Some(started) = idle_since {
          tokio::time::sleep_until(started + IDLE_TIMEOUT).await;
        } else {
          pending::<()>().await;
        }
      } => break,
    }
  }
  state.closed.send_replace(true);
  let _ = timeout(
    Duration::from_secs(1),
    poll_fn(|cx| connection.poll_close(cx)),
  )
  .await;
}

/// Negotiates the server side and dispatches independently flow-controlled services.
///
/// # Errors
/// Returns contract, framing, or physical transport errors. A failed logical
/// service closes its own stream while sibling services continue.
pub async fn serve<I, H, F>(mut io: I, handler: H) -> io::Result<()>
where
  I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
  H: Fn(Service, Stream) -> F + Send + Sync + 'static,
  F: Future<Output = io::Result<()>> + Send + 'static,
{
  timeout(STARTUP_TIMEOUT, async {
    io.write_all(PREFACE).await?;
    write_message(
      &mut io,
      &ProtocolOffer::new(
        PROTOCOL_BUILD,
        PROTOCOL_VERSION,
        SUPPORTED_PROTOCOL_VERSIONS,
      ),
    )
    .await?;
    let selection: Selection = read_message(&mut io).await?;
    if !SUPPORTED_PROTOCOL_VERSIONS.contains(&selection.protocol_version) {
      return Err(incompatible());
    }
    Ok(())
  })
  .await
  .map_err(|_| {
    io::Error::new(
      io::ErrorKind::TimedOut,
      "Shared transport negotiation timed out",
    )
  })??;
  let write_blocked = Arc::new(AtomicBool::new(false));
  let io = ObservedIo {
    inner: io,
    write_blocked: Arc::clone(&write_blocked),
    pause_reads_on_write: true,
  };
  // A reset closes a stream before its handler necessarily drops it. Yamux
  // treats inbound overflow as connection-fatal, so reserve one internal slot
  // to reject a replacement while keeping at most MAX_STREAMS handlers alive.
  let mut connection =
    yamux::Connection::new(io.compat(), config(MAX_STREAMS + 1), yamux::Mode::Server);
  let handler = Arc::new(handler);
  let mut services = JoinSet::new();
  let result = loop {
    tokio::select! {
      biased;
      _ = services.join_next(), if !services.is_empty() => {},
      incoming = poll_fn(|cx| {
        write_blocked.store(false, Ordering::Relaxed);
        connection.poll_next_inbound(cx)
      }) => {
        match incoming {
          Some(Ok(stream)) => {
            if services.len() >= MAX_STREAMS {
              drop(stream);
              continue;
            }
            let handler = Arc::clone(&handler);
            services.spawn(async move {
              let mut stream = Stream { inner: stream.compat(), _lifetime: None };
              let selector = timeout(STARTUP_TIMEOUT, stream.read_u8()).await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Shared service selection timed out"))??;
              handler(Service::decode(selector)?, stream).await
            });
          },
          Some(Err(error)) => break Err(io::Error::other(error)),
          None => break Ok(()),
        }
      },
    }
  };
  services.shutdown().await;
  result
}

fn config(max_streams: usize) -> yamux::Config {
  let mut config = yamux::Config::default();
  config
    .set_max_num_streams(max_streams)
    .set_max_connection_receive_window(Some(
      MAX_RECEIVE_BYTES + (max_streams - MAX_STREAMS) * MIN_STREAM_RECEIVE_BYTES,
    ))
    .set_read_after_close(false);
  config
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Selection {
  protocol_version: ProtocolVersion,
}

fn closed() -> io::Error {
  io::Error::new(
    io::ErrorKind::BrokenPipe,
    "The shared SSH service transport closed",
  )
}

fn incompatible() -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidData,
    "No shared published service transport contract",
  )
}

async fn read_message<T: DeserializeOwned>(io: &mut (impl AsyncRead + Unpin)) -> io::Result<T> {
  let size = io.read_u32().await? as usize;
  if size == 0 || size > MAX_NEGOTIATION_BYTES {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "Invalid shared transport negotiation frame size",
    ));
  }
  let mut bytes = vec![0; size];
  io.read_exact(&mut bytes).await?;
  serde_json::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

async fn write_message<T: Serialize>(
  io: &mut (impl AsyncWrite + Unpin),
  value: &T,
) -> io::Result<()> {
  let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
  if bytes.len() > MAX_NEGOTIATION_BYTES {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "Shared transport negotiation frame is too large",
    ));
  }
  io.write_u32(u32::try_from(bytes.len()).map_err(io::Error::other)?)
    .await?;
  io.write_all(&bytes).await?;
  io.flush().await
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  async fn echo_pair() -> (Client, tokio::task::JoinHandle<io::Result<()>>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(serve(server_io, |service, stream| async move {
      let (mut reader, mut writer) = tokio::io::split(stream);
      writer.write_u8(service as u8).await?;
      tokio::io::copy(&mut reader, &mut writer).await?;
      writer.shutdown().await
    }));
    (connect(client_io).await.unwrap(), server)
  }

  #[tokio::test]
  async fn many_services_share_one_connection_and_keep_half_closes_independent() {
    timeout(Duration::from_secs(5), async {
      let (client, server) = echo_pair().await;
      let mut streams = Vec::new();
      for index in 0..32 {
        let service = match index % 3 {
          0 => Service::Ctmux,
          1 => Service::Task,
          _ => Service::Udp,
        };
        let stream = client.open(service).await.unwrap();
        streams.push((stream, service, index));
      }
      for (mut stream, service, index) in streams {
        let payload = vec![index; 16_384];
        stream.write_all(&payload).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut echoed = Vec::new();
        stream.read_to_end(&mut echoed).await.unwrap();
        assert_eq!(echoed[0], service as u8);
        assert_eq!(&echoed[1..], payload);
      }
      assert!(!client.is_closed());
      client.close();
      client.closed().await;
      server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn a_slow_reader_and_a_dropped_stream_do_not_block_siblings() {
    timeout(Duration::from_secs(5), async {
      let (client_io, server_io) = tokio::io::duplex(64 * 1024);
      let server = tokio::spawn(serve(server_io, |service, mut stream| async move {
        if service == Service::Udp {
          stream.write_all(&vec![42; 2 * 1024 * 1024]).await?;
        } else {
          stream.write_all(b"responsive").await?;
        }
        stream.shutdown().await
      }));
      let client = connect(client_io).await.unwrap();
      let slow = client.open(Service::Udp).await.unwrap();
      let cancelled = client.open(Service::Task).await.unwrap();
      drop(cancelled);
      let mut sibling = client.open(Service::Ctmux).await.unwrap();
      let mut bytes = Vec::new();
      sibling.read_to_end(&mut bytes).await.unwrap();
      assert_eq!(bytes, b"responsive");
      drop(slow);
      assert!(!client.is_closed());
      client.close();
      server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn physical_loss_invalidates_the_handle_and_all_services() {
    let (client, server) = echo_pair().await;
    let mut stream = client.open(Service::Ctmux).await.unwrap();
    assert_eq!(stream.read_u8().await.unwrap(), Service::Ctmux as u8);
    server.abort();
    let _ = server.await;
    timeout(Duration::from_secs(1), client.closed())
      .await
      .unwrap();
    assert!(client.is_closed());
    assert!(client.open(Service::Task).await.is_err());
    let mut bytes = [0; 1];
    assert!(matches!(stream.read(&mut bytes).await, Ok(0) | Err(_)));
  }

  #[tokio::test]
  async fn active_streams_outlive_handles_and_idle_transports_close() {
    timeout(Duration::from_secs(8), async {
      let (client, server) = echo_pair().await;
      let mut stream = client.open(Service::Ctmux).await.unwrap();
      drop(client);
      assert_eq!(stream.read_u8().await.unwrap(), Service::Ctmux as u8);
      stream.write_all(b"still active").await.unwrap();
      stream.shutdown().await.unwrap();
      let mut echoed = Vec::new();
      stream.read_to_end(&mut echoed).await.unwrap();
      assert_eq!(echoed, b"still active");
      drop(stream);
      server.await.unwrap().unwrap();

      let (idle, server) = echo_pair().await;
      timeout(IDLE_TIMEOUT + Duration::from_secs(1), idle.closed())
        .await
        .unwrap();
      assert!(idle.open(Service::Ctmux).await.is_err());
      server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn unsupported_contracts_are_rejected_before_opening_services() {
    let (client_io, mut server_io) = tokio::io::duplex(8192);
    let peer = tokio::spawn(async move {
      server_io.write_all(PREFACE).await.unwrap();
      let unsupported = ProtocolVersion::new(2, 0, 1);
      write_message(
        &mut server_io,
        &ProtocolOffer::new(1, unsupported, &[unsupported]),
      )
      .await
      .unwrap();
    });
    let error = connect(client_io)
      .await
      .err()
      .expect("unsupported contract");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    peer.await.unwrap();
  }

  #[tokio::test]
  async fn stream_capacity_errors_preserve_existing_services() {
    timeout(Duration::from_secs(5), async {
      let (client, server) = echo_pair().await;
      let mut streams = Vec::new();
      for _ in 0..MAX_STREAMS {
        streams.push(client.open(Service::Ctmux).await.unwrap());
      }
      assert_eq!(
        client
          .open(Service::Task)
          .await
          .err()
          .expect("stream capacity")
          .kind(),
        io::ErrorKind::WouldBlock,
      );
      assert!(!client.is_closed());
      let mut stream = streams.pop().unwrap();
      assert_eq!(stream.read_u8().await.unwrap(), Service::Ctmux as u8);
      stream.write_all(b"existing").await.unwrap();
      let mut bytes = [0; 8];
      stream.read_exact(&mut bytes).await.unwrap();
      assert_eq!(&bytes, b"existing");
      client.close();
      server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn rapid_stream_replacement_at_capacity_preserves_siblings() {
    timeout(Duration::from_secs(10), async {
      // Small physical pipes keep queued writes backpressured while stream
      // drops and new admission race at the logical capacity boundary.
      let (client_io, server_io) = tokio::io::duplex(64);
      let server = tokio::spawn(serve(server_io, |_, stream| async move {
        let (mut reader, mut writer) = tokio::io::split(stream);
        tokio::io::copy(&mut reader, &mut writer).await?;
        writer.shutdown().await
      }));
      let client = connect(client_io).await.unwrap();
      let mut siblings = Vec::new();
      for _ in 0..MAX_STREAMS - 1 {
        siblings.push(client.open(Service::Ctmux).await.unwrap());
      }
      for _ in 0..256 {
        let mut replaced = client.open(Service::Task).await.unwrap();
        replaced.write_all(&[42; 1024]).await.unwrap();
        drop(replaced);
      }
      assert!(!client.is_closed());
      let mut sibling = siblings.pop().unwrap();
      sibling.write_all(b"healthy").await.unwrap();
      let mut bytes = [0; 7];
      sibling.read_exact(&mut bytes).await.unwrap();
      assert_eq!(&bytes, b"healthy");
      client.close();
      server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn delayed_service_cleanup_does_not_close_siblings_at_capacity() {
    timeout(Duration::from_secs(5), async {
      let (client_io, server_io) = tokio::io::duplex(64 * 1024);
      let server = tokio::spawn(serve(server_io, |service, stream| async move {
        if service == Service::Task {
          // Service setup can await a daemon or a destination while its
          // caller goes away. Keep ownership until that work is cancelled.
          tokio::time::sleep(Duration::from_millis(100)).await;
          drop(stream);
          return Ok(());
        }
        let (mut reader, mut writer) = tokio::io::split(stream);
        tokio::io::copy(&mut reader, &mut writer).await?;
        writer.shutdown().await
      }));
      let client = connect(client_io).await.unwrap();
      let mut siblings = Vec::new();
      for _ in 0..MAX_STREAMS - 1 {
        siblings.push(client.open(Service::Ctmux).await.unwrap());
      }
      for _ in 0..16 {
        if let Ok(mut replaced) = client.open(Service::Task).await {
          let _ = replaced.write_all(b"start").await;
        }
        assert!(!client.is_closed());
      }
      let mut sibling = siblings.pop().unwrap();
      sibling.write_all(b"healthy").await.unwrap();
      let mut bytes = [0; 7];
      sibling.read_exact(&mut bytes).await.unwrap();
      assert_eq!(&bytes, b"healthy");
      assert!(!client.is_closed());
      client.close();
      server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
  }

  #[derive(Default)]
  struct ReadGate {
    paused: AtomicBool,
    waiter: std::sync::Mutex<Option<std::task::Waker>>,
  }

  impl ReadGate {
    fn release(&self) {
      self.paused.store(false, Ordering::Relaxed);
      if let Some(waiter) = self.waiter.lock().unwrap().take() {
        waiter.wake();
      }
    }
  }

  struct GatedReader {
    inner: tokio::io::DuplexStream,
    gate: Arc<ReadGate>,
  }

  impl AsyncRead for GatedReader {
    fn poll_read(
      mut self: Pin<&mut Self>,
      cx: &mut Context<'_>,
      buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
      let mut waiter = self.gate.waiter.lock().unwrap();
      if self.gate.paused.load(Ordering::Relaxed) {
        *waiter = Some(cx.waker().clone());
        return Poll::Pending;
      }
      drop(waiter);
      Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
  }

  impl AsyncWrite for GatedReader {
    fn poll_write(
      mut self: Pin<&mut Self>,
      cx: &mut Context<'_>,
      bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
      Pin::new(&mut self.inner).poll_write(cx, bytes)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
      Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
      Pin::new(&mut self.inner).poll_shutdown(cx)
    }
  }

  #[tokio::test]
  async fn rejected_streams_under_physical_write_pressure_preserve_siblings() {
    timeout(Duration::from_secs(10), async {
      let (client_io, server_io) = tokio::io::duplex(64);
      let server = tokio::spawn(serve(server_io, |service, mut stream| async move {
        if service == Service::Task {
          stream.write_all(&vec![42; 65_536]).await?;
          tokio::time::sleep(Duration::from_millis(100)).await;
          return Ok(());
        }
        let (mut reader, mut writer) = tokio::io::split(stream);
        tokio::io::copy(&mut reader, &mut writer).await?;
        writer.shutdown().await
      }));
      let gate = Arc::new(ReadGate::default());
      let client = connect(GatedReader {
        inner: client_io,
        gate: Arc::clone(&gate),
      })
      .await
      .unwrap();
      let mut siblings = Vec::new();
      for _ in 0..MAX_STREAMS - 1 {
        siblings.push(client.open(Service::Ctmux).await.unwrap());
      }
      gate.paused.store(true, Ordering::Relaxed);
      let pressure = client.open(Service::Task).await.unwrap();
      // Keep the stream alive until its output fills the physical pipe. Its
      // reset and subsequent rejected replacements then race a pending write.
      tokio::time::sleep(Duration::from_millis(50)).await;
      drop(pressure);
      let churn_client = client.clone();
      let churn = tokio::spawn(async move {
        for _ in 0..256 {
          let _ = churn_client.open(Service::Task).await;
          assert!(!churn_client.is_closed());
        }
      });
      tokio::time::sleep(Duration::from_millis(50)).await;
      assert!(!client.is_closed());
      gate.release();
      churn.await.unwrap();
      let mut sibling = siblings.pop().unwrap();
      sibling.write_all(b"healthy").await.unwrap();
      let mut bytes = [0; 7];
      sibling.read_exact(&mut bytes).await.unwrap();
      assert_eq!(&bytes, b"healthy");
      assert!(!client.is_closed());
      client.close();
      server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
  }
}
