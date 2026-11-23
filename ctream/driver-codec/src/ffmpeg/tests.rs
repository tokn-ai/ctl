use super::{Decoder, Encoder, available_decoders};
use ctream_driver::{CaptureLifetime, Frame};
use ctream_proto::{CaptureFormat, VideoCodec, VideoOptions, VideoQuality};
use std::{
  io::{self, Write},
  process::{Command, Stdio},
  sync::mpsc,
  thread,
  time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(10);

fn ffmpeg_installed() -> bool {
  Command::new("ffmpeg")
    .arg("-version")
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .is_ok_and(|status| status.success())
}

fn ppm(output: &mut impl Write, [width, height]: [usize; 2], color: [u8; 3]) -> io::Result<()> {
  write!(output, "P6\n{width} {height}\n255\n")?;
  output.write_all(&color.repeat(width * height))
}

fn assert_frame(frame: &Frame, size: [usize; 2], color: [u8; 3], codec: VideoCodec) {
  assert_eq!(frame.size, size);
  assert_eq!(frame.pixels.len(), size[0] * size[1] * 3);
  for pixel in [&frame.pixels[..3], &frame.pixels[frame.pixels.len() - 3..]] {
    for (actual, expected) in pixel.iter().zip(color) {
      assert!(
        actual.abs_diff(expected) < 35,
        "{} RGB mismatch: {pixel:?} vs {color:?}",
        codec.label()
      );
    }
  }
}

#[test]
fn live_round_trip_resizes_and_crops_for_each_available_codec() {
  if !ffmpeg_installed() {
    eprintln!("FFmpeg not installed");
    return;
  }
  let decoders = available_decoders().unwrap();
  for codec in [VideoCodec::Av1, VideoCodec::H264] {
    let options = VideoOptions {
      codec,
      ..VideoOptions::default()
    };
    let Ok(encoder) = Encoder::open(options, &[codec]) else {
      eprintln!("{} encoder not installed", codec.label());
      continue;
    };
    assert!(decoders.contains(&codec));
    assert_eq!(encoder.info().codec, codec);
    let sizes = [[65, 49], [83, 67], [23, 11]];
    let colors = [[180, 45, 34], [29, 153, 116], [84, 30, 185]];
    let (input, mut wire) = io::pipe().unwrap();
    let (arrived, ready) = mpsc::channel();
    let (cancel, cancelled) = mpsc::channel();
    let worker = thread::spawn(move || {
      let lifetime = CaptureLifetime::from_receiver(cancelled);
      encoder
        .run(CaptureFormat::Ppm, &mut wire, &lifetime, |output| {
          for (size, color) in sizes.into_iter().zip(colors) {
            ppm(output, size, color)?;
            // A complete frame must arrive while both FFmpeg inputs are open.
            ready.recv_timeout(TIMEOUT)?;
          }
          Ok(())
        })
        .map_err(|error| error.to_string())
    });
    let mut decoder = Decoder::open(input, move || {
      let _ = cancel.send(());
    })
    .unwrap();
    let mut frame = Frame::default();
    for (size, color) in sizes.into_iter().zip(colors) {
      let delivered = decoder.try_read_frame(&mut frame).unwrap();
      assert!(delivered, "{} frame did not arrive", codec.label());
      assert_frame(&frame, size, color, codec);
      arrived.send(()).unwrap();
    }
    assert!(!decoder.try_read_frame(&mut frame).unwrap());
    worker.join().unwrap().unwrap();
  }
}

#[test]
fn low_quality_scales_live_capture_and_caps_frame_rate() {
  if !ffmpeg_installed() {
    eprintln!("FFmpeg not installed");
    return;
  }
  let codec = VideoCodec::H264;
  let options = VideoOptions {
    codec,
    quality: VideoQuality::Low,
    max_fps: 1,
  };
  let Ok(encoder) = Encoder::open(options, &[codec]) else {
    eprintln!("H.264 encoder not installed");
    return;
  };
  assert_eq!(encoder.info().quality, VideoQuality::Low);
  assert_eq!(encoder.info().max_fps, 1);
  let size = [1501, 750];
  let colors = [[200, 50, 40], [40, 75, 180]];
  let (input, mut wire) = io::pipe().unwrap();
  let (arrived, ready) = mpsc::channel();
  let (started, start_time) = mpsc::channel();
  let (cancel, cancelled) = mpsc::channel();
  let worker = thread::spawn(move || {
    let lifetime = CaptureLifetime::from_receiver(cancelled);
    encoder
      .run(CaptureFormat::Ppm, &mut wire, &lifetime, |output| {
        started.send(Instant::now())?;
        for color in colors {
          ppm(output, size, color)?;
          ready.recv_timeout(TIMEOUT)?;
        }
        Ok(())
      })
      .map_err(|error| error.to_string())
  });
  let mut decoder = Decoder::open(input, move || {
    let _ = cancel.send(());
  })
  .unwrap();
  let started = start_time.recv_timeout(TIMEOUT).unwrap();
  let mut frame = Frame::default();
  for color in colors {
    assert!(decoder.try_read_frame(&mut frame).unwrap());
    // Preserve the aspect ratio within one pixel and remove the even-size
    // padding required by the codec. Check both edges for clipped pixels.
    assert_frame(&frame, [1280, 639], color, codec);
    arrived.send(()).unwrap();
  }
  assert!(started.elapsed() >= Duration::from_secs(1));
  assert!(!decoder.try_read_frame(&mut frame).unwrap());
  worker.join().unwrap().unwrap();
}

#[test]
fn h264_fallback_cancels_an_idle_encoder_on_decoder_drop() {
  if !ffmpeg_installed() {
    eprintln!("FFmpeg not installed");
    return;
  }
  let Ok(encoder) = Encoder::open(VideoOptions::default(), &[VideoCodec::H264]) else {
    eprintln!("H.264 encoder not installed");
    return;
  };
  assert_eq!(encoder.info().codec, VideoCodec::H264);
  let (input, mut wire) = io::pipe().unwrap();
  let (cancel, cancelled) = mpsc::channel();
  let (done, finished) = mpsc::channel();
  let worker = thread::spawn(move || {
    let lifetime = CaptureLifetime::from_receiver(cancelled);
    let result = encoder.run(CaptureFormat::Ppm, &mut wire, &lifetime, |output| {
      ppm(output, [128, 72], [16, 187, 143])?;
      let deadline = Instant::now() + TIMEOUT;
      while !lifetime.cancelled(Duration::from_millis(20)) {
        if Instant::now() >= deadline {
          return Err(io::Error::new(io::ErrorKind::TimedOut, "capture did not cancel").into());
        }
      }
      Ok(())
    });
    let _ = done.send(result.map_err(|error| error.to_string()));
  });
  let mut decoder = Decoder::open(input, move || {
    let _ = cancel.send(());
  })
  .unwrap();
  assert!(decoder.try_read_frame(&mut Frame::default()).unwrap());
  drop(decoder);
  finished.recv_timeout(TIMEOUT).unwrap().unwrap();
  worker.join().unwrap();
}
