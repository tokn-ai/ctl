use crate::transport::{Cancellation, Channel};
pub use ctream_driver::Frame;
use ctream_driver_codec::{ffmpeg::Decoder, raw};
use ctream_proto::{
  CAPTURE_STARTED_LIMIT, CaptureFormat, CaptureStarted, VideoCodec, VideoInfo, VideoRequest,
  VideoStarted, valid_capture_driver,
};
use serde::de::DeserializeOwned;
use std::io::{self, BufRead, BufReader, Read, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameFormat {
  FixedRgb { size: [usize; 2] },
  Ppm,
}

impl FrameFormat {
  fn raw(self) -> CaptureFormat {
    match self {
      Self::FixedRgb { size } => CaptureFormat::FixedRgb { size },
      Self::Ppm => CaptureFormat::Ppm,
    }
  }
}

pub trait VideoSource {
  type Buffer: AsRef<[u8]>;

  fn read_frame(&mut self, frame: &mut Frame<Self::Buffer>) -> io::Result<()>;
}

/// A capture owns both frame delivery and the channel that keeps its producer alive.
pub struct Capture<R: Read, W: Write> {
  // Fields drop in declaration order: close stdin before a process reader's
  // destructor can request cancellation of the capture producer.
  lifetime: CaptureLifetime<W>,
  frames: FrameReader<R>,
  driver: Option<String>,
  video: Option<VideoInfo>,
}

pub struct CaptureLifetime<W: Write> {
  writer: Option<W>,
  cancellation: Cancellation,
}

impl<W: Write> CaptureLifetime<W> {
  /// Signal the end of capture while its reader drains to EOF. This lets a
  /// caller wait for the remote target to be released before opening it again.
  pub(crate) fn close_writer(&mut self) {
    drop(self.writer.take());
  }
}

impl<W: Write> Drop for CaptureLifetime<W> {
  fn drop(&mut self) {
    self.close_writer();
    self.cancellation.cancel();
  }
}

impl<R: Read, W: Write> Capture<R, W> {
  pub fn new(channel: Channel<R, W>, format: FrameFormat) -> Self {
    Self {
      lifetime: CaptureLifetime {
        writer: Some(channel.writer),
        cancellation: channel.cancellation,
      },
      frames: FrameReader::new(channel.reader, format),
      driver: None,
      video: None,
    }
  }

  pub fn driver(&self) -> Option<&str> {
    self.driver.as_deref()
  }

  pub fn format(&self) -> FrameFormat {
    self.frames.format
  }

  pub fn video(&self) -> Option<&VideoInfo> {
    self.video.as_ref()
  }

  /// Keep the handshake reader for frames: its buffer may already contain the
  /// first image. Failed negotiation closes stdin before requesting cancellation.
  pub(crate) fn started(channel: Channel<R, W>, requested: &str) -> io::Result<Self> {
    let Channel {
      reader,
      writer,
      cancellation,
    } = channel;
    let mut reader = BufReader::new(reader);
    // Locals drop in reverse declaration order on every handshake error.
    // The lifetime must close stdin before the buffered reader is dropped.
    let lifetime = CaptureLifetime {
      writer: Some(writer),
      cancellation,
    };
    let started: CaptureStarted = read_start(&mut reader)?;
    if !valid_capture_driver(&started.driver)
      || started.driver == "auto"
      || (requested != "auto" && started.driver != requested)
    {
      return Err(io::Error::other(
        "agent reported an unexpected capture driver",
      ));
    }
    let format = match started.format {
      CaptureFormat::Ppm => FrameFormat::Ppm,
      CaptureFormat::FixedRgb { size } => {
        raw::pixel_count(size)?;
        FrameFormat::FixedRgb { size }
      }
    };
    Ok(Self {
      lifetime,
      frames: FrameReader {
        reader: Reader::Raw(raw::FrameReader::from_buffered(reader, format.raw())),
        format,
      },
      driver: Some(started.driver),
      video: None,
    })
  }

  /// Retain the writer with the viewer. Closing it tells the agent to stop capture
  /// and close the frame stream, allowing the reader worker to finish.
  pub fn split(self) -> (FrameReader<R>, CaptureLifetime<W>) {
    (self.frames, self.lifetime)
  }
}

fn read_start<H: DeserializeOwned>(reader: &mut impl BufRead) -> io::Result<H> {
  let mut header = String::new();
  reader
    .by_ref()
    .take((CAPTURE_STARTED_LIMIT + 1) as u64)
    .read_line(&mut header)?;
  if header.len() > CAPTURE_STARTED_LIMIT || !header.ends_with('\n') {
    return Err(io::Error::other(
      "invalid or oversized capture startup handshake",
    ));
  }
  serde_json::from_str(&header).map_err(io::Error::other)
}

impl<R: Read + Send + 'static, W: Write> Capture<R, W> {
  pub(crate) fn started_video(
    channel: Channel<R, W>,
    requested: &str,
    request: &VideoRequest,
  ) -> io::Result<Self> {
    let Channel {
      reader,
      writer,
      cancellation,
    } = channel;
    let mut reader = BufReader::new(reader);
    let lifetime = CaptureLifetime {
      writer: Some(writer),
      cancellation,
    };
    let started: VideoStarted = read_start(&mut reader)?;
    if !valid_capture_driver(&started.driver)
      || started.driver == "auto"
      || (requested != "auto" && started.driver != requested)
      || started.video.codec == VideoCodec::Auto
      || !request.codecs.contains(&started.video.codec)
      || !valid_capture_driver(&started.video.encoder)
      || started.video.quality != request.options.quality
      || started.video.max_fps == 0
      || started.video.max_fps > request.options.max_fps
    {
      return Err(io::Error::other("agent reported unexpected video settings"));
    }
    let cancellation = lifetime.cancellation.clone();
    let decoder = Decoder::open(reader, move || cancellation.cancel())?;
    Ok(Self {
      lifetime,
      frames: FrameReader {
        reader: Reader::Video(decoder),
        format: FrameFormat::Ppm,
      },
      driver: Some(started.driver),
      video: Some(started.video),
    })
  }
}

impl<R: Read, W: Write> VideoSource for Capture<R, W> {
  type Buffer = Vec<u8>;

  fn read_frame(&mut self, frame: &mut Frame<Self::Buffer>) -> io::Result<()> {
    self.frames.read_frame(frame)
  }
}

pub struct FrameReader<R: Read> {
  reader: Reader<R>,
  format: FrameFormat,
}

enum Reader<R: Read> {
  Raw(raw::FrameReader<R>),
  Video(Decoder),
}

impl<R: Read> FrameReader<R> {
  pub fn new(reader: R, format: FrameFormat) -> Self {
    Self {
      reader: Reader::Raw(raw::FrameReader::new(reader, format.raw())),
      format,
    }
  }
}

impl<R: Read> FrameReader<R> {
  /// Reads the next frame, returning false for clean EOF between frames.
  /// An incomplete header or pixel buffer is still an error.
  pub fn try_read_frame(&mut self, frame: &mut Frame) -> io::Result<bool> {
    match &mut self.reader {
      Reader::Raw(reader) => reader.try_read_frame(frame),
      Reader::Video(decoder) => decoder.try_read_frame(frame),
    }
  }
}

impl<R: Read> VideoSource for FrameReader<R> {
  type Buffer = Vec<u8>;

  fn read_frame(&mut self, frame: &mut Frame<Self::Buffer>) -> io::Result<()> {
    if self.try_read_frame(frame)? {
      return Ok(());
    }
    Err(match self.format {
      FrameFormat::FixedRgb { .. } => {
        io::Error::new(io::ErrorKind::UnexpectedEof, "failed to fill whole buffer")
      }
      FrameFormat::Ppm => io::Error::other("missing or incomplete PPM frame header"),
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::{Arc, Mutex};

  type Events = Arc<Mutex<Vec<&'static str>>>;

  struct RecordingReader {
    bytes: io::Cursor<Vec<u8>>,
    events: Events,
    cancellation: Cancellation,
    fail: bool,
  }

  impl Read for RecordingReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
      if self.fail {
        return Err(io::Error::other("capture handshake read failed"));
      }
      self.bytes.read(output)
    }
  }

  impl Drop for RecordingReader {
    fn drop(&mut self) {
      self.events.lock().unwrap().push("reader");
      // ProcessReader also requests cancellation when its reader is dropped.
      self.cancellation.cancel();
    }
  }

  struct RecordingWriter(Events);

  impl Write for RecordingWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
      Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  impl Drop for RecordingWriter {
    fn drop(&mut self) {
      self.0.lock().unwrap().push("writer");
    }
  }

  fn recording_channel(bytes: Vec<u8>) -> (Channel<RecordingReader, RecordingWriter>, Events) {
    let events = Events::default();
    let cancelled = Arc::clone(&events);
    let cancellation = Cancellation::new(move || cancelled.lock().unwrap().push("cancel"));
    (
      Channel {
        reader: RecordingReader {
          bytes: io::Cursor::new(bytes),
          events: Arc::clone(&events),
          cancellation: cancellation.clone(),
          fail: false,
        },
        writer: RecordingWriter(Arc::clone(&events)),
        cancellation,
      },
      events,
    )
  }

  #[test]
  fn invalid_capture_startup_closes_writer_before_cancellation_and_reader() {
    for header in [
      "".into(),
      "{}\n".into(),
      "x".repeat(CAPTURE_STARTED_LIMIT + 1),
      "{\"driver\":\"provider\",\"format\":{\"type\":\"ppm\"}}".into(),
      "{\"driver\":\"auto\",\"format\":{\"type\":\"ppm\"}}\n".into(),
      "{\"driver\":\"other\",\"format\":{\"type\":\"ppm\"}}\n".into(),
      "{\"driver\":\"provider\",\"format\":{\"type\":\"unknown\"}}\n".into(),
      "{\"driver\":\"provider\",\"format\":{\"type\":\"fixed_rgb\",\"size\":[0,1]}}\n".into(),
    ] {
      let (channel, events) = recording_channel(header.into_bytes());
      assert!(Capture::started(channel, "provider").is_err());
      assert_eq!(*events.lock().unwrap(), ["writer", "cancel", "reader"]);
    }
    let (mut channel, events) = recording_channel(Vec::new());
    channel.reader.fail = true;
    assert!(Capture::started(channel, "provider").is_err());
    assert_eq!(*events.lock().unwrap(), ["writer", "cancel", "reader"]);
  }

  #[test]
  fn invalid_video_startup_closes_writer_before_cancellation_and_reader() {
    let request = VideoRequest {
      options: ctream_proto::VideoOptions::default(),
      codecs: vec![VideoCodec::Av1, VideoCodec::H264],
    };
    let valid = VideoStarted {
      driver: "provider".into(),
      video: VideoInfo {
        codec: VideoCodec::Av1,
        encoder: "libsvtav1".into(),
        quality: request.options.quality,
        max_fps: request.options.max_fps,
      },
    };
    let mut invalid = Vec::new();
    for driver in ["auto", "other", "bad driver"] {
      invalid.push(VideoStarted {
        driver: driver.into(),
        ..valid.clone()
      });
    }
    for video in [
      VideoInfo {
        codec: VideoCodec::Auto,
        ..valid.video.clone()
      },
      VideoInfo {
        encoder: "bad encoder".into(),
        ..valid.video.clone()
      },
      VideoInfo {
        quality: ctream_proto::VideoQuality::Low,
        ..valid.video.clone()
      },
      VideoInfo {
        max_fps: 0,
        ..valid.video.clone()
      },
      VideoInfo {
        max_fps: 31,
        ..valid.video.clone()
      },
    ] {
      invalid.push(VideoStarted {
        video,
        ..valid.clone()
      });
    }
    for started in invalid {
      let bytes = format!("{}\n", serde_json::to_string(&started).unwrap()).into_bytes();
      let (channel, events) = recording_channel(bytes);
      assert!(Capture::started_video(channel, "provider", &request).is_err());
      assert_eq!(*events.lock().unwrap(), ["writer", "cancel", "reader"]);
    }
    let (channel, events) = recording_channel(b"{}\n".to_vec());
    assert!(Capture::started_video(channel, "provider", &request).is_err());
    assert_eq!(*events.lock().unwrap(), ["writer", "cancel", "reader"]);
  }

  #[test]
  fn dropping_capture_closes_writer_before_cancellation_and_reader() {
    for negotiated in [false, true] {
      let mut bytes = Vec::new();
      if negotiated {
        bytes.extend_from_slice(b"{\"driver\":\"provider\",\"format\":{\"type\":\"ppm\"}}\n");
      }
      bytes.extend_from_slice(b"P6\n1 1\n255\nabc");
      let (channel, events) = recording_channel(bytes);
      let mut capture = if negotiated {
        Capture::started(channel, "provider").unwrap()
      } else {
        Capture::new(channel, FrameFormat::Ppm)
      };
      assert_eq!(capture.driver(), negotiated.then_some("provider"));
      let mut frame = Frame::default();
      capture.read_frame(&mut frame).unwrap();
      assert_eq!(frame.pixels, b"abc");
      assert!(events.lock().unwrap().is_empty());
      drop(capture);
      assert_eq!(*events.lock().unwrap(), ["writer", "cancel", "reader"]);
    }
  }

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
  fn distinguishes_clean_end_from_incomplete_frame() {
    for (format, complete, incomplete) in [
      (
        FrameFormat::Ppm,
        &b"P6\n1 1\n255\nabc"[..],
        &b"P6\n1 1\n255\nab"[..],
      ),
      (
        FrameFormat::FixedRgb { size: [1, 1] },
        &b"abc"[..],
        &b"ab"[..],
      ),
    ] {
      let mut stream = FrameReader::new(complete, format);
      let mut frame = Frame::default();
      assert!(stream.try_read_frame(&mut frame).unwrap());
      assert!(!stream.try_read_frame(&mut frame).unwrap());
      assert_eq!(frame.size, [1, 1]);
      assert_eq!(frame.pixels, b"abc");

      let mut stream = FrameReader::new(incomplete, format);
      assert_eq!(
        stream.try_read_frame(&mut frame).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
      );
    }
    for malformed in [&b"P6"[..], &b"P5\n1 1\n255\nabc"[..]] {
      let mut stream = FrameReader::new(malformed, FrameFormat::Ppm);
      assert!(stream.try_read_frame(&mut Frame::default()).is_err());
    }
    let mut stream = FrameReader::new(io::empty(), FrameFormat::FixedRgb { size: [0, 1] });
    assert!(stream.try_read_frame(&mut Frame::default()).is_err());
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
