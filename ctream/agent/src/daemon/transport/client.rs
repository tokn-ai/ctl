//! A host relay drives its physical lane even when the synchronous SDK agent is
//! idle. Reading a SWITCH never depends on the next screenshot or input call.
use super::{
  MAX_REQUEST,
  records::{self, Output},
};
use crate::daemon::{IO_TIMEOUT, Opening, endpoint, generation, invalid, read_json, write_json};
use interprocess::local_socket::tokio::Stream as Socket;
use std::{
  io,
  path::{Path, PathBuf},
  pin::Pin,
  sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
  },
  task::{Context, Poll, ready},
  time::Duration,
};
use tokio::{
  io::{
    self as aio, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
    DuplexStream, ReadBuf,
  },
  runtime::{Builder, Runtime},
  task::JoinHandle,
  time::{Instant, sleep, timeout, timeout_at},
};
use uuid::Uuid;

const RESUME_TIMEOUT: Duration = Duration::from_secs(20);
type Failure = Arc<Mutex<Option<io::Error>>>;

pub(in crate::daemon) fn runtime() -> io::Result<&'static Runtime> {
  static TRANSPORT: OnceLock<io::Result<Runtime>> = OnceLock::new();
  TRANSPORT
    .get_or_init(|| {
      Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("ctream-lane-io")
        .enable_all()
        .build()
    })
    .as_ref()
    .map_err(|e| io::Error::new(e.kind(), e.to_string()))
}

pub(in crate::daemon) enum Stream {
  Legacy(Socket),
  Lane(Lane),
}

pub(in crate::daemon) struct Lane {
  io: DuplexStream,
  pump: JoinHandle<()>,
  failure: Failure,
  epoch: Arc<AtomicU64>,
}
impl Drop for Lane {
  fn drop(&mut self) {
    self.pump.abort();
  }
}

impl Stream {
  pub(in crate::daemon) fn reply_deadline(&self, normal: Duration) -> ReplyDeadline {
    let epoch = match self {
      Self::Legacy(_) => None,
      Self::Lane(lane) => Some((lane.epoch.clone(), lane.epoch.load(Ordering::Acquire))),
    };
    ReplyDeadline { normal, epoch }
  }

  pub(in crate::daemon) fn discard_pending(&self) {
    if let Self::Legacy(stream) = self {
      endpoint::discard_pending(stream);
    }
  }
  #[cfg(target_os = "linux")]
  pub(in crate::daemon) fn delegate_fd(&self) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsFd;
    match self {
      Self::Legacy(Socket::UdSocket(socket)) => socket.as_fd().try_clone_to_owned(),
      Self::Lane(_) => Err(invalid(
        "driver requires a process-owned physical registration",
      )),
    }
  }
}

/// Preserve the ordinary command deadline on a healthy or stalled host. Only
/// a request that overlapped a reserved or committed handoff gets extra time.
pub(in crate::daemon) struct ReplyDeadline {
  normal: Duration,
  epoch: Option<(Arc<AtomicU64>, u64)>,
}
impl ReplyDeadline {
  pub(in crate::daemon) async fn wait<T>(
    self,
    operation: impl Future<Output = T>,
  ) -> Result<T, tokio::time::error::Elapsed> {
    let deadline = Instant::now() + self.normal;
    tokio::pin!(operation);
    match timeout_at(deadline, &mut operation).await {
      Ok(result) => Ok(result),
      Err(elapsed) => {
        let moving = self.epoch.is_some_and(|(epoch, start)| {
          !start.is_multiple_of(2)
            || epoch.load(Ordering::Acquire) != start
            || standby_running().unwrap_or(false)
        });
        if !moving {
          return Err(elapsed);
        }
        timeout_at(
          deadline + generation::HANDOFF_TIMEOUT + RESUME_TIMEOUT,
          operation,
        )
        .await
      }
    }
  }
}

impl AsyncRead for Stream {
  fn poll_read(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    match self.get_mut() {
      Self::Legacy(socket) => Pin::new(socket).poll_read(cx, buf),
      Self::Lane(lane) => {
        let was = buf.filled().len();
        ready!(Pin::new(&mut lane.io).poll_read(cx, buf))?;
        if was == buf.filled().len()
          && let Some(error) = lane.failure.lock().unwrap().take()
        {
          return Poll::Ready(Err(error));
        }
        Poll::Ready(Ok(()))
      }
    }
  }
}
impl AsyncWrite for Stream {
  fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
    match self.get_mut() {
      Self::Legacy(socket) => Pin::new(socket).poll_write(cx, buf),
      Self::Lane(lane) => Pin::new(&mut lane.io).poll_write(cx, buf),
    }
  }
  fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    match self.get_mut() {
      Self::Legacy(socket) => Pin::new(socket).poll_flush(cx),
      Self::Lane(lane) => Pin::new(&mut lane.io).poll_flush(cx),
    }
  }
  fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    match self.get_mut() {
      Self::Legacy(socket) => Pin::new(socket).poll_shutdown(cx),
      Self::Lane(lane) => Pin::new(&mut lane.io).poll_shutdown(cx),
    }
  }
}

pub(in crate::daemon) async fn connect(
  executable: Option<&Path>,
  existing_only: bool,
) -> io::Result<Stream> {
  // Create and register the physical stream on the persistent reactor as well.
  let executable = executable.map(PathBuf::from);
  runtime()?
    .spawn(open(executable, existing_only))
    .await
    .map_err(io::Error::other)?
}

fn standby_running() -> io::Result<bool> {
  match endpoint::lock("upgrade.lock")?.try_lock_shared() {
    Ok(()) => Ok(false),
    Err(std::fs::TryLockError::WouldBlock) => Ok(true),
    Err(std::fs::TryLockError::Error(error)) => Err(error),
  }
}

async fn open(executable: Option<PathBuf>, existing_only: bool) -> io::Result<Stream> {
  let lane = Uuid::new_v4();
  let deadline = Instant::now() + RESUME_TIMEOUT;
  let mut pending_upgrade = false;
  loop {
    let opened = if existing_only {
      endpoint::connect_existing().await
    } else {
      endpoint::connect_via(executable.as_deref()).await
    };
    let socket = match opened {
      Ok(socket) => socket,
      Err(error)
        if transient(&error)
          && Instant::now() < deadline
          && (pending_upgrade || standby_running()?) =>
      {
        // A viewer can first open its control lane while its existing observer
        // is moving. Wait for the host's elected standby through that gap.
        pending_upgrade = true;
        sleep(Duration::from_millis(25)).await;
        continue;
      }
      Err(error) => return Err(error),
    };
    let mut socket = BufReader::new(socket);
    match handshake(&mut socket, lane, false).await {
      Ok(Some(answer)) if answer == "ready" => {
        let (client, relay) = aio::duplex(MAX_REQUEST);
        let failure: Failure = Arc::default();
        let errors = failure.clone();
        let epoch = Arc::new(AtomicU64::new(0));
        let physical_epoch = epoch.clone();
        let pump = tokio::spawn(async move {
          let (read, mut write) = aio::split(relay);
          let mut read = BufReader::new(read);
          if let Err(error) = bridge(lane, socket, &mut read, &mut write, &physical_epoch).await {
            *errors.lock().unwrap() = Some(error);
          }
        });
        return Ok(Stream::Lane(Lane {
          io: client,
          pump,
          failure,
          epoch,
        }));
      }
      Ok(Some(answer)) if answer == "pending" && Instant::now() < deadline => {
        pending_upgrade = true;
        sleep(Duration::from_millis(25)).await;
      }
      // A connection queued in the predecessor's backlog can be reset as the
      // successor takes the endpoint. It hasn't carried any application bytes.
      result
        if (matches!(&result, Ok(None)) || result.as_ref().is_err_and(transient))
          && Instant::now() < deadline
          && (pending_upgrade || standby_running()?) =>
      {
        pending_upgrade = true;
        sleep(Duration::from_millis(25)).await;
      }
      // Older daemons reject the unknown hello before any session Opening or
      // mutation is sent. The fallback opens one ordinary legacy connection.
      Ok(None) => return Ok(Stream::Legacy(endpoint::connect_existing().await?)),
      Err(error)
        if matches!(
          error.kind(),
          io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
        ) =>
      {
        return Ok(Stream::Legacy(endpoint::connect_existing().await?));
      }
      Err(error) => return Err(error),
      _ => return Err(io::Error::other("desktop daemon declined the lane")),
    }
  }
}

async fn handshake(
  stream: &mut BufReader<Socket>,
  lane: Uuid,
  resume: bool,
) -> io::Result<Option<String>> {
  write_json(
    stream.get_mut(),
    &Opening::Lane {
      format: generation::FORMAT,
      lane,
      resume,
    },
  )
  .await?;
  timeout(IO_TIMEOUT, read_json(stream, 64)).await?
}

async fn resume(lane: Uuid) -> io::Result<BufReader<Socket>> {
  let deadline = Instant::now() + RESUME_TIMEOUT;
  timeout_at(deadline, async {
    loop {
      // A SWITCH never launches an arbitrary call-side executable. Only the
      // selected successor can restore this private capability.
      match endpoint::connect_existing().await {
        Ok(socket) => {
          let mut reader = BufReader::new(socket);
          match handshake(&mut reader, lane, true).await {
            Ok(Some(answer)) if answer == "ready" => return Ok(reader),
            Ok(Some(answer)) if answer == "pending" => (),
            Ok(None) => (),
            Err(error) if transient(&error) => (),
            Err(error) => return Err(error),
            _ => return Err(invalid("successor did not recognize the logical session")),
          }
        }
        Err(error) if transient(&error) => {
          if let Err(recovery) = generation::recover_disconnected().await
            && !transient(&recovery)
          {
            return Err(recovery);
          }
        }
        Err(error) => return Err(error),
      }
      sleep(Duration::from_millis(25)).await;
    }
  })
  .await?
}

fn transient(error: &io::Error) -> bool {
  matches!(
    error.kind(),
    io::ErrorKind::NotFound
      | io::ErrorKind::ConnectionRefused
      | io::ErrorKind::ConnectionReset
      | io::ErrorKind::BrokenPipe
      | io::ErrorKind::UnexpectedEof
      | io::ErrorKind::TimedOut
      | io::ErrorKind::WouldBlock
  )
}

async fn bridge(
  lane: Uuid,
  mut socket: BufReader<Socket>,
  input: &mut (impl aio::AsyncBufRead + Unpin),
  output: &mut (impl AsyncWrite + Unpin),
  physical_epoch: &AtomicU64,
) -> io::Result<()> {
  let mut pending = Vec::new();
  let mut assembling = Vec::new();
  loop {
    let switch = epoch(&mut socket, input, output, &mut pending, &mut assembling).await?;
    if !switch {
      return Ok(());
    }
    physical_epoch.fetch_add(1, Ordering::AcqRel);
    endpoint::discard_pending(socket.get_ref());
    drop(socket);
    socket = resume(lane).await?;
    physical_epoch.fetch_add(1, Ordering::AcqRel);
  }
}

// Advance one physical write at a time. A large request can fill a named pipe
// while the predecessor is parked. Reading SWITCH must remain possible then.
async fn send_part(
  writer: &mut (impl AsyncWrite + Unpin),
  request: &[u8],
  written: usize,
) -> io::Result<usize> {
  let prefix = (request.len() as u32).to_le_bytes();
  let bytes = if written < prefix.len() {
    &prefix[written..]
  } else {
    &request[written - prefix.len()..]
  };
  match writer.write(bytes).await? {
    0 => Err(io::ErrorKind::WriteZero.into()),
    n => Ok(n),
  }
}

/// Retain one request until the daemon explicitly reports that it consumed
/// its complete record. On SWITCH, the sole unconsumed request may cross the
/// boundary; a consumed mutation is never transmitted again.
async fn epoch(
  socket: &mut (impl AsyncRead + AsyncWrite + Unpin),
  input: &mut (impl aio::AsyncBufRead + Unpin),
  output: &mut (impl AsyncWrite + Unpin),
  pending: &mut Vec<u8>,
  assembling: &mut Vec<u8>,
) -> io::Result<bool> {
  let (mut read, mut write) = aio::split(socket);
  let mut sent = u64::from(!pending.is_empty());
  let mut acked = 0;
  let mut write_error = None;
  let mut written = 0;
  let mut data = Vec::new();
  loop {
    let frame = {
      let incoming = records::receive(&mut read, &mut data);
      tokio::pin!(incoming);
      loop {
        if pending.is_empty() {
          let remaining = MAX_REQUEST + 1 - assembling.len();
          let mut limited = (&mut *input).take(remaining as u64);
          tokio::select! {
            biased;
            result = &mut incoming => break result?,
            count = limited.read_until(b'\n', assembling) => {
              if count? == 0 && assembling.is_empty() {return Ok(false)}
              if assembling.len() > MAX_REQUEST || assembling.last() != Some(&b'\n') {
                return Err(invalid("incomplete or oversized lane request"));
              }
              std::mem::swap(pending, assembling);
              sent = sent.checked_add(1).ok_or_else(|| invalid("lane sequence exhausted"))?;
              written = 0;
            }
          }
        } else if written < 4 + pending.len() && write_error.is_none() {
          tokio::select! {
            biased;
            result = &mut incoming => break result?,
            result = send_part(&mut write, pending, written) => match result {
              Ok(n) => written += n,
              Err(error) => write_error = Some(error),
            }
          }
        } else {
          // A final SWITCH may already be buffered when a write first observes
          // a closed predecessor. Consume that authenticated result before
          // classifying the loss. Never infer a handoff from the write error.
          break if write_error.is_some() {
            timeout(IO_TIMEOUT, &mut incoming).await??
          } else {
            incoming.await?
          };
        }
      }
    };
    let consumed = match frame {
      Output::Data { consumed } | Output::Switch { consumed } => consumed,
      Output::End => return write_error.map_or(Ok(false), Err),
    };
    if !(acked..=sent).contains(&consumed) {
      return Err(invalid("invalid lane acknowledgement"));
    }
    acked = consumed;
    if consumed == sent {
      pending.clear();
    }
    match frame {
      Output::Switch { .. } => return Ok(true),
      Output::Data { .. } => output.write_all(&data).await?,
      Output::End => unreachable!(),
    }
  }
}

#[cfg(test)]
mod tests;
