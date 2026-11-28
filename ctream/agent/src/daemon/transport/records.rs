use super::{HEADER, MAX_DATA, MAX_REQUEST};
use crate::daemon::invalid;
use std::{
  io,
  pin::Pin,
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
  },
  task::{Context, Poll, ready},
};
use tokio::io::{
  AsyncBufRead, AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, ReadBuf,
};

/// Each physical epoch starts at offset zero. The first epoch carries the
/// opening as a request. Resumed epochs start directly at the next request.
pub(in crate::daemon) fn server<R, W>(read: R, write: W) -> (RecordReader<R>, RecordWriter<W>) {
  let consumed = Arc::new(AtomicU64::new(0));
  (
    RecordReader {
      read,
      consumed: consumed.clone(),
      prefix: [0; 4],
      header_bytes: 0,
      body: Vec::new(),
      filled: 0,
      pos: 0,
    },
    RecordWriter {
      write,
      consumed,
      header: [0; HEADER],
      header_bytes: HEADER,
      remaining: 0,
    },
  )
}

pub(in crate::daemon) struct RecordReader<R> {
  read: R,
  consumed: Arc<AtomicU64>,
  prefix: [u8; 4],
  header_bytes: usize,
  body: Vec<u8>,
  filled: usize,
  pos: usize,
}

impl<R> RecordReader<R> {
  pub(in crate::daemon) fn consumed(&self) -> u64 {
    self.consumed.load(Ordering::Relaxed)
  }
}

impl<R: AsyncRead + Unpin> RecordReader<R> {
  /// A switched relay drops this physical socket before dialing the successor.
  /// Drain bytes that raced the barrier without admitting any of them.
  /// The generic local-socket flush is a no-op on Windows, so observing peer
  /// close keeps the SWITCH alive until its recipient has read it.
  pub(in crate::daemon) async fn confirm_switch(&mut self) -> io::Result<()> {
    tokio::time::timeout(crate::daemon::IO_TIMEOUT, async {
      let mut tail = (&mut self.read).take((MAX_REQUEST + 5) as u64);
      match tokio::io::copy(&mut tail, &mut tokio::io::sink()).await {
        Ok(n) if n <= (MAX_REQUEST + 4) as u64 => Ok(()),
        Ok(_) => Err(invalid("unexpected data after lane handoff")),
        Err(error)
          if matches!(
            error.kind(),
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
          ) =>
        {
          Ok(())
        }
        Err(error) => Err(error),
      }
    })
    .await?
  }
}

fn poll_bytes<R: AsyncRead + Unpin>(
  reader: &mut R,
  cx: &mut Context<'_>,
  bytes: &mut [u8],
) -> Poll<io::Result<usize>> {
  let mut buf = ReadBuf::new(bytes);
  ready!(Pin::new(reader).poll_read(cx, &mut buf))?;
  Poll::Ready(Ok(buf.filled().len()))
}

impl<R: AsyncRead + Unpin> AsyncBufRead for RecordReader<R> {
  fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<&[u8]>> {
    let this = self.get_mut();
    while this.header_bytes < this.prefix.len() {
      let n = ready!(poll_bytes(
        &mut this.read,
        cx,
        &mut this.prefix[this.header_bytes..]
      ))?;
      if n == 0 {
        return if this.header_bytes == 0 {
          Poll::Ready(Ok(&[]))
        } else {
          Poll::Ready(Err(invalid("incomplete lane request header")))
        };
      }
      this.header_bytes += n;
    }
    if this.body.is_empty() {
      let len = u32::from_le_bytes(this.prefix) as usize;
      if !(1..=MAX_REQUEST).contains(&len) {
        return Poll::Ready(Err(invalid("invalid lane request length")));
      }
      this.body.resize(len, 0);
    }
    while this.filled < this.body.len() {
      let n = ready!(poll_bytes(
        &mut this.read,
        cx,
        &mut this.body[this.filled..]
      ))?;
      if n == 0 {
        return Poll::Ready(Err(invalid("incomplete lane request")));
      }
      this.filled += n;
      if this.filled == this.body.len()
        && (this.body.last() != Some(&b'\n') || this.body[..this.filled - 1].contains(&b'\n'))
      {
        return Poll::Ready(Err(invalid("expected one complete lane request")));
      }
    }
    Poll::Ready(Ok(&this.body[this.pos..]))
  }

  fn consume(self: Pin<&mut Self>, amt: usize) {
    let this = self.get_mut();
    assert!(amt <= this.filled - this.pos);
    this.pos += amt;
    if amt > 0 && this.pos == this.body.len() {
      this.consumed.fetch_add(1, Ordering::Relaxed);
      this.body.clear();
      this.header_bytes = 0;
      this.filled = 0;
      this.pos = 0;
    }
  }
}

impl<R: AsyncRead + Unpin> AsyncRead for RecordReader<R> {
  fn poll_read(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    let data = ready!(self.as_mut().poll_fill_buf(cx))?;
    let n = data.len().min(buf.remaining());
    buf.put_slice(&data[..n]);
    self.consume(n);
    Poll::Ready(Ok(()))
  }
}

pub(in crate::daemon) struct RecordWriter<W> {
  write: W,
  consumed: Arc<AtomicU64>,
  header: [u8; HEADER],
  header_bytes: usize,
  remaining: usize,
}

fn header(length: usize, consumed: u64) -> [u8; HEADER] {
  let mut header = [0; HEADER];
  header[..4].copy_from_slice(&(length as u32).to_le_bytes());
  header[4..].copy_from_slice(&consumed.to_le_bytes());
  header
}

impl<W: AsyncWrite + Unpin> RecordWriter<W> {
  /// Call only at an application boundary, after the successor has accepted
  /// the checkpoint. The server then awaits the peer closing this physical lane.
  pub(in crate::daemon) async fn switch(&mut self, consumed: u64) -> io::Result<()> {
    if self.remaining != 0 {
      return Err(invalid("cannot switch within a lane response"));
    }
    self.write.write_all(&header(0, consumed)).await?;
    self.write.flush().await
  }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for RecordWriter<W> {
  fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
    let this = self.get_mut();
    if buf.is_empty() {
      return Poll::Ready(Ok(0));
    }
    if this.remaining == 0 {
      this.remaining = buf.len().min(MAX_DATA);
      this.header = header(this.remaining, this.consumed.load(Ordering::Relaxed));
      this.header_bytes = 0;
    }
    while this.header_bytes < HEADER {
      let n = ready!(Pin::new(&mut this.write).poll_write(cx, &this.header[this.header_bytes..]))?;
      if n == 0 {
        return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
      }
      this.header_bytes += n;
    }
    let n =
      ready!(Pin::new(&mut this.write).poll_write(cx, &buf[..this.remaining.min(buf.len())]))?;
    this.remaining -= n;
    Poll::Ready(Ok(n))
  }

  fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Pin::new(&mut self.get_mut().write).poll_flush(cx)
  }
  fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Pin::new(&mut self.get_mut().write).poll_shutdown(cx)
  }
}

pub(super) enum Output {
  End,
  Data { consumed: u64 },
  Switch { consumed: u64 },
}

pub(super) async fn receive(
  reader: &mut (impl AsyncRead + Unpin),
  data: &mut Vec<u8>,
) -> io::Result<Output> {
  let mut header = [0; HEADER];
  if reader.read(&mut header[..1]).await? == 0 {
    return Ok(Output::End);
  }
  reader.read_exact(&mut header[1..]).await?;
  let length = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
  let consumed = u64::from_le_bytes(header[4..].try_into().unwrap());
  if length == 0 {
    return Ok(Output::Switch { consumed });
  }
  if length > MAX_DATA {
    return Err(invalid("invalid lane response length"));
  }
  data.resize(length, 0);
  reader.read_exact(data).await?;
  Ok(Output::Data { consumed })
}
