use crate::transport::{Cancellation, Channel};
use std::io::{self, BufRead, BufReader, Read, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameFormat {
  FixedRgb { size: [usize; 2] },
  Ppm,
}

#[derive(Default)]
pub struct Frame<B = Vec<u8>> {
  pub size: [usize; 2],
  pub pixels: B,
}

pub trait VideoSource {
  type Buffer: AsRef<[u8]>;

  fn read_frame(&mut self, frame: &mut Frame<Self::Buffer>) -> io::Result<()>;
}

/// A capture owns both frame delivery and the channel that keeps its producer alive.
pub struct Capture<R: Read, W: Write> {
  frames: FrameReader<R>,
  lifetime: CaptureLifetime<W>,
}

pub struct CaptureLifetime<W: Write> {
  writer: Option<W>,
  cancellation: Cancellation,
}

impl<W: Write> Drop for CaptureLifetime<W> {
  fn drop(&mut self) {
    drop(self.writer.take());
    self.cancellation.cancel();
  }
}

impl<R: Read, W: Write> Capture<R, W> {
  pub fn new(channel: Channel<R, W>, format: FrameFormat) -> Self {
    Self {
      frames: FrameReader::new(channel.reader, format),
      lifetime: CaptureLifetime {
        writer: Some(channel.writer),
        cancellation: channel.cancellation,
      },
    }
  }

  /// Retain the writer with the viewer. Closing it tells the agent to stop capture
  /// and close the frame stream, allowing the reader worker to finish.
  pub fn split(self) -> (FrameReader<R>, CaptureLifetime<W>) {
    (self.frames, self.lifetime)
  }
}

impl<R: Read, W: Write> VideoSource for Capture<R, W> {
  type Buffer = Vec<u8>;

  fn read_frame(&mut self, frame: &mut Frame<Self::Buffer>) -> io::Result<()> {
    self.frames.read_frame(frame)
  }
}

pub struct FrameReader<R: Read> {
  reader: BufReader<R>,
  format: FrameFormat,
}

impl<R: Read> FrameReader<R> {
  pub fn new(reader: R, format: FrameFormat) -> Self {
    Self {
      reader: BufReader::new(reader),
      format,
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

fn pixel_count([width, height]: [usize; 2]) -> io::Result<usize> {
  if !(1..=16384).contains(&width)
    || !(1..=16384).contains(&height)
    || width * height > 32 * 1024 * 1024
  {
    return Err(io::Error::other("invalid frame size"));
  }
  Ok(width * height)
}

impl<R: Read> VideoSource for FrameReader<R> {
  type Buffer = Vec<u8>;

  fn read_frame(&mut self, frame: &mut Frame<Self::Buffer>) -> io::Result<()> {
    let size = match self.format {
      FrameFormat::FixedRgb { size } => size,
      FrameFormat::Ppm => {
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
        size
      }
    };
    frame.pixels.resize(pixel_count(size)? * 3, 0);
    self.reader.read_exact(&mut frame.pixels)?;
    frame.size = size;
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn successive_frames_resize_and_keep_binary_pixels() {
    let bytes = b"P6\n1 1\n255\n\x20\x0a\x00P6\n2 1\n255\nabcdef";
    let mut source = FrameReader::new(&bytes[..], FrameFormat::Ppm);
    let mut frame = Frame::default();
    source.read_frame(&mut frame).unwrap();
    assert_eq!(frame.size, [1, 1]);
    assert_eq!(frame.pixels, b"\x20\x0a\x00");
    source.read_frame(&mut frame).unwrap();
    assert_eq!(frame.size, [2, 1]);
    assert_eq!(frame.pixels, b"abcdef");
    assert!(source.read_frame(&mut frame).is_err());
  }

  #[test]
  fn rejects_unbounded_and_incomplete_frames() {
    for bytes in [
      &b"P6\n0 1\n255\n"[..],
      &b"P6\n16384 16384\n255\n"[..],
      &b"P6\n18446744073709551615 2\n255\n"[..],
      &b"P6\n1 1 1\n255\n"[..],
      &b"P6\n1 1\n256\nabc"[..],
      &b"P6\n1 1\n255\nab"[..],
    ] {
      assert!(
        FrameReader::new(bytes, FrameFormat::Ppm)
          .read_frame(&mut Frame::default())
          .is_err()
      );
    }
    assert!(
      FrameReader::new(
        io::empty(),
        FrameFormat::FixedRgb {
          size: [usize::MAX, 2]
        }
      )
      .read_frame(&mut Frame::default())
      .is_err()
    );
  }

  #[cfg(unix)]
  #[test]
  fn dropping_lifetime_releases_a_waiting_capture_worker() {
    use std::{os::unix::net::UnixStream, thread, time::Duration};

    let (frames, mut producer) = UnixStream::pair().unwrap();
    let (lifetime, mut control) = UnixStream::pair().unwrap();
    frames
      .set_read_timeout(Some(Duration::from_secs(2)))
      .unwrap();
    control
      .set_read_timeout(Some(Duration::from_secs(2)))
      .unwrap();
    let backend = thread::spawn(move || {
      let mut request = Vec::new();
      control.read_to_end(&mut request).unwrap();
      producer.write_all(b"P6\n1 1\n255\nabc").unwrap();
    });
    let capture = Capture::new(
      Channel {
        reader: frames,
        writer: lifetime,
        cancellation: Cancellation::default(),
      },
      FrameFormat::Ppm,
    );
    let (mut source, lifetime) = capture.split();
    let worker = thread::spawn(move || {
      let mut frame = Frame::default();
      source.read_frame(&mut frame).unwrap();
      assert_eq!(frame.pixels, b"abc");
      assert!(source.read_frame(&mut frame).is_err());
    });
    drop(lifetime);
    backend.join().unwrap();
    worker.join().unwrap();
  }
}
