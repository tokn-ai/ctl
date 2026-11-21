use crate::transport::{Cancellation, Channel};
use ctream_proto::{PING_HELLO, PING_TOKEN};
use std::{
  io::{self, Read, Write},
  time::{Duration, Instant},
};

/// Measures round-trip time to the agent over one persistent transport channel.
pub struct LatencyProbe<R, W> {
  reader: R,
  writer: Option<W>,
  cancellation: Cancellation,
  ready: bool,
}

impl<R: Read, W: Write> LatencyProbe<R, W> {
  pub(crate) fn new(channel: Channel<R, W>) -> Self {
    Self {
      reader: channel.reader,
      writer: Some(channel.writer),
      cancellation: channel.cancellation,
      ready: false,
    }
  }

  /// Wait for a reply and return its round-trip time. The first sample also
  /// waits for an untimed handshake. A failed sample closes the channel.
  pub fn sample(&mut self) -> io::Result<Duration> {
    let result = self.exchange();
    if result.is_err() {
      self.close();
    }
    result
  }

  fn exchange(&mut self) -> io::Result<Duration> {
    let writer = self
      .writer
      .as_mut()
      .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "latency probe is closed"))?;
    let mut byte = [0];
    if !self.ready {
      self.reader.read_exact(&mut byte)?;
      if byte != [PING_HELLO] {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "invalid latency probe handshake",
        ));
      }
      self.ready = true;
    }

    let started = Instant::now();
    writer.write_all(&[PING_TOKEN])?;
    writer.flush()?;
    self.reader.read_exact(&mut byte)?;
    if byte != [PING_TOKEN] {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid latency probe reply",
      ));
    }
    Ok(started.elapsed())
  }
}

impl<R, W> LatencyProbe<R, W> {
  fn close(&mut self) {
    drop(self.writer.take());
    self.cancellation.cancel();
  }
}

impl<R, W> Drop for LatencyProbe<R, W> {
  fn drop(&mut self) {
    self.close();
  }
}
