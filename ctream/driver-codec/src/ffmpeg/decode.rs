use super::{Latest, coded_size, command, process};
use crate::raw::FrameReader;
use ctream_driver::Frame;
use ctream_proto::{
  CaptureFormat,
  video::{self, VideoPacket},
};
use std::{
  io::{self, Read, Write},
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
  },
  thread,
};

pub struct Decoder {
  frames: Arc<Latest>,
  control: Arc<Control>,
}

struct Control {
  stopped: AtomicBool,
  close_input: Box<dyn Fn() + Send + Sync>,
}

impl Control {
  fn is_stopped(&self) -> bool {
    self.stopped.load(Ordering::Acquire)
  }

  fn stop(&self) {
    if !self.stopped.swap(true, Ordering::AcqRel) {
      (self.close_input)();
    }
  }

  fn fail(&self, frames: &Latest, error: io::Error) {
    if !self.is_stopped() {
      frames.finish(Err(error));
      self.stop();
    }
  }
}

impl Decoder {
  /// The cancellation callback closes the underlying transport if a read is pending.
  ///
  /// # Errors
  /// Returns an I/O error if the decode worker cannot be started.
  pub fn open<R, F>(mut reader: R, on_drop: F) -> io::Result<Self>
  where
    R: Read + Send + 'static,
    F: Fn() + Send + Sync + 'static,
  {
    let frames = Arc::new(Latest::default());
    let control = Arc::new(Control {
      stopped: AtomicBool::new(false),
      close_input: Box::new(on_drop),
    });
    let worker_frames = Arc::clone(&frames);
    let worker_control = Arc::clone(&control);
    thread::Builder::new()
      .name("ctream-video-decoder".into())
      .spawn(
        move || match stream(&mut reader, &worker_frames, &worker_control) {
          Ok(()) => worker_frames.finish(Ok(())),
          Err(error) => worker_control.fail(&worker_frames, error),
        },
      )?;
    Ok(Self { frames, control })
  }

  /// Wait for the latest decoded frame or return `false` at the end of the stream.
  ///
  /// # Errors
  /// Returns errors reading packets or decoding video.
  pub fn try_read_frame(&mut self, frame: &mut Frame) -> io::Result<bool> {
    self.frames.next(frame, None, || false)
  }
}

impl Drop for Decoder {
  fn drop(&mut self) {
    self.control.stop();
  }
}

fn stream(reader: &mut impl Read, frames: &Latest, control: &Control) -> io::Result<()> {
  if control.is_stopped() {
    return Ok(());
  }
  let first = video::read_packet(reader)?.ok_or_else(missing_end)?;
  let mut current = Some(first);
  while !control.is_stopped() {
    match current {
      Some(VideoPacket::Segment { size }) => {
        current = segment(reader, frames, control, size)?;
      }
      Some(VideoPacket::End) | None => return Ok(()),
      Some(VideoPacket::Data(_)) => {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "video data before segment",
        ));
      }
    }
  }
  Ok(())
}

fn segment(
  reader: &mut impl Read,
  frames: &Latest,
  control: &Control,
  display: [usize; 2],
) -> io::Result<Option<VideoPacket>> {
  let coded = coded_size(display);
  video::pixel_count(coded)?;
  let (child, mut input, output, stderr) = process::Process::start(command::decoder())?;
  let feeding_done = AtomicBool::new(false);
  let stop_child = AtomicBool::new(false);
  thread::scope(|scope| {
    let diagnostics = scope.spawn(|| process::diagnostics(stderr));
    let wait = scope.spawn(|| {
      let result = (|| {
        let status = child.wait(|| control.is_stopped() || stop_child.load(Ordering::Acquire))?;
        let diagnostics = process::join(diagnostics)?;
        process::check_exit(status, "decoder", &diagnostics)
      })();
      if !feeding_done.load(Ordering::Acquire) {
        let error = result.err().unwrap_or_else(|| {
          io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "FFmpeg decoder exited during capture",
          )
        });
        control.fail(frames, error);
        return Ok(());
      }
      result
    });
    let decode = scope.spawn(|| {
      let mut reader = FrameReader::new(output, CaptureFormat::Ppm);
      let mut frame = Frame::default();
      let result = (|| {
        while !control.is_stopped() && reader.try_read_frame(&mut frame)? {
          crop(&mut frame, coded, display)?;
          frames.publish(&mut frame);
        }
        Ok(())
      })();
      if let Err(error) = result {
        control.fail(frames, error);
      }
    });
    let next = forward(reader, &mut input, control);
    feeding_done.store(true, Ordering::Release);
    if next.is_err() {
      stop_child.store(true, Ordering::Release);
    }
    drop(input);
    process::join(decode)?;
    let status = process::join(wait)?;
    if control.is_stopped() {
      return Ok(None);
    }
    let next = next?;
    status?;
    Ok(next)
  })
}

fn forward(
  reader: &mut impl Read,
  input: &mut impl Write,
  control: &Control,
) -> io::Result<Option<VideoPacket>> {
  while !control.is_stopped() {
    match video::read_packet(reader)?.ok_or_else(missing_end)? {
      VideoPacket::Data(bytes) => input.write_all(&bytes)?,
      next => return Ok(Some(next)),
    }
  }
  Ok(None)
}

fn missing_end() -> io::Error {
  io::Error::new(
    io::ErrorKind::UnexpectedEof,
    "encoded capture ended without an end packet",
  )
}

fn crop(frame: &mut Frame, coded: [usize; 2], display: [usize; 2]) -> io::Result<()> {
  if frame.size != coded {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "decoded frame has unexpected dimensions",
    ));
  }
  let width = display[0] * 3;
  let stride = coded[0] * 3;
  if width != stride {
    for row in 1..display[1] {
      let start = row * stride;
      frame.pixels.copy_within(start..start + width, row * width);
    }
  }
  frame.pixels.truncate(width * display[1]);
  frame.size = display;
  Ok(())
}
