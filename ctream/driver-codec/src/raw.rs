//! Read RGB24 frames from capture drivers and codec subprocesses.

use ctream_driver::Frame;
use ctream_proto::CaptureFormat;
use std::io::{self, BufRead, BufReader, Read};

pub struct FrameReader<R: Read> {
  reader: BufReader<R>,
  format: CaptureFormat,
}

impl<R: Read> FrameReader<R> {
  pub fn new(reader: R, format: CaptureFormat) -> Self {
    Self::from_buffered(BufReader::new(reader), format)
  }

  /// Keeps any frame bytes prefetched while reading a stream handshake.
  pub fn from_buffered(reader: BufReader<R>, format: CaptureFormat) -> Self {
    Self { reader, format }
  }

  /// Returns false at clean EOF between frames. A partial frame is an error.
  ///
  /// # Errors
  /// Returns an error if a frame is malformed, truncated, or unreadable.
  pub fn try_read_frame(&mut self, frame: &mut Frame) -> io::Result<bool> {
    let (size, count) = match self.format {
      CaptureFormat::FixedRgb { size } => {
        let count = pixel_count(size)?;
        if self.at_eof()? {
          return Ok(false);
        }
        (size, count)
      }
      CaptureFormat::Ppm => {
        if self.at_eof()? {
          return Ok(false);
        }
        if header_line(&mut self.reader)?.trim_end() != "P6" {
          return Err(io::Error::other("expected a P6 RGB frame"));
        }
        let dimensions = header_line(&mut self.reader)?;
        let mut parts = dimensions.split_ascii_whitespace();
        let mut dimension = || {
          parts
            .next()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| io::Error::other("invalid PPM frame size"))
        };
        let size = [dimension()?, dimension()?];
        if parts.next().is_some() || header_line(&mut self.reader)?.trim_end() != "255" {
          return Err(io::Error::other("unsupported PPM frame"));
        }
        (size, pixel_count(size)?)
      }
    };
    frame.pixels.resize(count * 3, 0);
    self.reader.read_exact(&mut frame.pixels)?;
    frame.size = size;
    Ok(true)
  }

  fn at_eof(&mut self) -> io::Result<bool> {
    loop {
      match self.reader.fill_buf() {
        Ok(bytes) => return Ok(bytes.is_empty()),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
        Err(error) => return Err(error),
      }
    }
  }
}

fn header_line(reader: &mut impl BufRead) -> io::Result<String> {
  let mut line = String::new();
  reader.take(64).read_line(&mut line)?;
  if !line.ends_with('\n') {
    return Err(io::Error::other("missing or incomplete PPM frame header"));
  }
  Ok(line)
}

/// Validates frame dimensions and returns the count of RGB pixels.
///
/// # Errors
/// Returns an error for empty dimensions or a frame exceeding the size limit.
pub fn pixel_count(size: [usize; 2]) -> io::Result<usize> {
  ctream_proto::video::pixel_count(size).map_err(|_| io::Error::other("invalid frame size"))
}
