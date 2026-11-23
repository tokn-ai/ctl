use super::{Latest, coded_size, command, process};
use crate::raw::FrameReader;
use ctream_driver::{CaptureCancellation, CaptureLifetime, Frame, Result};
use ctream_proto::{CaptureFormat, VideoCodec, VideoInfo, VideoOptions, video};
use std::{
  io::{self, PipeWriter, Read, Write},
  sync::atomic::{AtomicBool, Ordering},
  thread,
  time::{Duration, Instant},
};

pub struct Encoder {
  options: VideoOptions,
  name: &'static str,
}

impl Encoder {
  /// Select a usable encoder from the codecs the receiving viewer supports.
  ///
  /// # Errors
  /// Returns an error if video options are invalid, `FFmpeg` cannot start, or
  /// none of the negotiated encoders are usable.
  pub fn open(options: VideoOptions, accepted: &[VideoCodec]) -> io::Result<Self> {
    Self::open_cancellable(options, accepted, &CaptureCancellation::default())
  }

  /// Select a usable encoder while observing the capture's cancellation token.
  ///
  /// # Errors
  /// Returns an interrupted error if cancelled, or the same errors as [`Self::open`].
  pub fn open_cancellable(
    options: VideoOptions,
    accepted: &[VideoCodec],
    cancellation: &CaptureCancellation,
  ) -> io::Result<Self> {
    let name = command::select_encoder(options, accepted, cancellation)?;
    Ok(Self { options, name })
  }

  #[must_use]
  pub fn info(&self) -> VideoInfo {
    VideoInfo {
      codec: command::encoder_codec(self.name),
      encoder: self.name.into(),
      quality: self.options.quality,
      max_fps: self.options.max_fps,
    }
  }

  /// Encode the source frames written by `capture` and stream the video packets.
  ///
  /// # Errors
  /// Returns any capture, encoding, or output I/O error encountered.
  pub fn run<W: Write + Send, F: FnOnce(&mut PipeWriter) -> Result<()>>(
    self,
    format: CaptureFormat,
    output: &mut W,
    lifetime: &CaptureLifetime,
    capture: F,
  ) -> Result<()> {
    let (reader, writer) = io::pipe()?;
    let frames = Latest::default();
    let cancellation = lifetime.cancellation();
    let (capture, stream) = thread::scope(|scope| {
      let receive = scope.spawn(|| {
        let mut reader = FrameReader::new(reader, format);
        let mut frame = Frame::default();
        let result = (|| {
          while !cancellation.is_cancelled() && reader.try_read_frame(&mut frame)? {
            frames.publish(&mut frame);
          }
          Ok(())
        })();
        frames.finish(result);
      });
      let stream_output = &mut *output;
      let stream = scope.spawn(|| {
        let result = self.stream(&frames, stream_output, &cancellation);
        if result.is_err() {
          cancellation.cancel();
        }
        result
      });
      // Drop the producer's pipe even if the capture callback unwinds.
      let mut writer = writer;
      let capture = capture(&mut writer);
      drop(writer);
      let stream = process::join(stream).and_then(|result| result);
      (
        capture,
        stream.and_then(|started| process::join(receive).map(|()| started)),
      )
    });
    let started = stream?;
    capture?;
    if started && !cancellation.is_cancelled() {
      video::write_end(output)?;
    }
    Ok(())
  }

  fn stream(
    &self,
    frames: &Latest,
    output: &mut impl Write,
    cancellation: &CaptureCancellation,
  ) -> io::Result<bool> {
    let mut frame = Frame::default();
    let mut started = false;
    if !frames.next(&mut frame, None, || cancellation.is_cancelled())? {
      return Ok(started);
    }
    while !cancellation.is_cancelled() {
      let (emitted, resized) = self.segment(frames, &mut frame, output, cancellation)?;
      started |= emitted;
      if !resized {
        break;
      }
    }
    Ok(started)
  }

  fn segment(
    &self,
    frames: &Latest,
    frame: &mut Frame,
    output: &mut impl Write,
    cancellation: &CaptureCancellation,
  ) -> io::Result<(bool, bool)> {
    let source = frame.size;
    let display = display_size(source, self.options.quality.max_dimension());
    let coded = coded_size(display);
    let interval = Duration::from_secs_f64(1.0 / f64::from(self.options.max_fps));
    let command = command::encoder(self.options, self.name, source, coded, display);
    let (child, mut input, mut bytes, stderr) = process::Process::start(command)?;
    let feeding_done = AtomicBool::new(false);
    let stop_child = AtomicBool::new(false);
    let exited = AtomicBool::new(false);
    thread::scope(|scope| {
      let diagnostics = scope.spawn(|| process::diagnostics(stderr));
      let wait = scope.spawn(|| {
        let status =
          child.wait(|| cancellation.is_cancelled() || stop_child.load(Ordering::Acquire));
        let early = !feeding_done.load(Ordering::Acquire);
        exited.store(true, Ordering::Release);
        (status, early)
      });
      let feed = scope.spawn(|| {
        let stopped = || cancellation.is_cancelled() || exited.load(Ordering::Acquire);
        let result = (|| {
          while !stopped() {
            let ready_at = Instant::now() + interval;
            input.write_all(&frame.pixels)?;
            if !frames.next(frame, Some(ready_at), stopped)? {
              return Ok(false);
            }
            if frame.size != source {
              return Ok(true);
            }
          }
          Ok(false)
        })();
        feeding_done.store(true, Ordering::Release);
        drop(input);
        result
      });
      let emitted = pump(&mut bytes, output, display);
      if emitted.is_err() {
        stop_child.store(true, Ordering::Release);
      }
      let (status, early) = process::join(wait)?;
      let resized: io::Result<bool> = process::join(feed)?;
      let diagnostics = process::join(diagnostics)?;
      if cancellation.is_cancelled() {
        return Ok((false, false));
      }
      let emitted = emitted?;
      process::check_exit(status?, "encoder", &diagnostics)?;
      let resized = resized?;
      if early {
        return Err(io::Error::other("FFmpeg encoder exited during capture"));
      }
      Ok((emitted, resized))
    })
  }
}

fn pump(input: &mut impl Read, output: &mut impl Write, size: [usize; 2]) -> io::Result<bool> {
  let mut started = false;
  let mut chunk = vec![0; video::MAX_VIDEO_CHUNK];
  loop {
    let count = match input.read(&mut chunk) {
      Ok(0) => return Ok(started),
      Ok(count) => count,
      Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
      Err(error) => return Err(error),
    };
    if !started {
      video::write_segment(output, size)?;
      started = true;
    }
    video::write_data(output, &chunk[..count])?;
  }
}

fn display_size(size: [usize; 2], limit: usize) -> [usize; 2] {
  let longest = size[0].max(size[1]);
  if longest <= limit {
    size
  } else {
    size.map(|dimension| (dimension * limit / longest).max(1))
  }
}
