//! The initial `ConPTY` cursor query belongs to the daemon, not an attachment.
use std::io;

const CURSOR_QUERY: &[u8] = b"\x1b[6n";
const MAX_STARTUP_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct StartupOutput(Vec<u8>);
impl StartupOutput {
  fn push(&mut self, bytes: &[u8]) -> io::Result<Option<Vec<u8>>> {
    if self.0.len().saturating_add(bytes.len()) > MAX_STARTUP_BYTES {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "ConPTY startup output exceeded its bound",
      ));
    }
    self.0.extend_from_slice(bytes);
    let Some(offset) = self
      .0
      .windows(CURSOR_QUERY.len())
      .position(|bytes| bytes == CURSOR_QUERY)
    else {
      return Ok(None);
    };
    self.0.drain(offset..offset + CURSOR_QUERY.len());
    Ok(Some(std::mem::take(&mut self.0)))
  }
}

#[cfg(windows)]
pub(crate) struct PtyIo {
  pub reader: Box<dyn io::Read + Send>,
  pub writer: Box<dyn io::Write + Send>,
  pub initial_output: Vec<u8>,
}

#[cfg(windows)]
pub(crate) fn initialize(
  mut reader: Box<dyn io::Read + Send>,
  mut writer: Box<dyn io::Write + Send>,
) -> io::Result<PtyIo> {
  use std::sync::mpsc;
  use std::time::Duration;
  let (send, receive) = mpsc::sync_channel(1);
  std::thread::Builder::new()
    .name("rmux-conpty-startup".into())
    .spawn(move || {
      let result = (|| {
        let mut startup = StartupOutput::default();
        let mut bytes = [0; 4096];
        loop {
          let length = match reader.read(&mut bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
          };
          if length == 0 {
            return Err(io::Error::new(
              io::ErrorKind::UnexpectedEof,
              "ConPTY closed during startup",
            ));
          }
          if let Some(initial_output) = startup.push(&bytes[..length])? {
            // Every new canonical rmux terminal starts at row 1, column 1.
            writer.write_all(b"\x1b[1;1R")?;
            writer.flush()?;
            return Ok(PtyIo {
              reader,
              writer,
              initial_output,
            });
          }
        }
      })();
      let _ = send.send(result);
    })?;
  receive
    .recv_timeout(Duration::from_secs(5))
    .map_err(|error| {
      io::Error::new(
        match error {
          mpsc::RecvTimeoutError::Timeout => io::ErrorKind::TimedOut,
          mpsc::RecvTimeoutError::Disconnected => io::ErrorKind::BrokenPipe,
        },
        "ConPTY cursor handshake did not complete",
      )
    })?
}

#[cfg(test)]
mod tests {
  use super::StartupOutput;

  #[test]
  fn fragmented_query_is_consumed_without_losing_surrounding_output() {
    for split in 0..=4 {
      let mut startup = StartupOutput::default();
      let query = b"\x1b[6n";
      assert!(startup.push(b"prefix").unwrap().is_none());
      let first = startup.push(&query[..split]).unwrap();
      if split == 4 {
        assert_eq!(first.unwrap(), b"prefix");
      } else {
        assert!(first.is_none());
        let tail = [&query[split..], b"suffix"].concat();
        assert_eq!(startup.push(&tail).unwrap().unwrap(), b"prefixsuffix");
      }
    }
  }

  #[test]
  fn unrelated_sequences_do_not_complete_startup_and_output_is_bounded() {
    let mut startup = StartupOutput::default();
    assert!(startup.push(b"\x1b[?6n\x1b[5n").unwrap().is_none());
    assert!(startup.push(&vec![b'x'; super::MAX_STARTUP_BYTES]).is_err());
  }
}
