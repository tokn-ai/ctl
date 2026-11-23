use super::process;
use ctream_driver::CaptureCancellation;
use ctream_proto::video::{VideoCodec, VideoOptions};
use std::{
  io::{self, Read, Write},
  process::Command,
  thread,
  time::{Duration, Instant},
};

const AV1_ENCODERS: &[&str] = &["libsvtav1", "libaom-av1"];
const H264_ENCODERS: &[&str] = &["libx264", "h264_videotoolbox"];
const PROBE_SIZE: [usize; 2] = [64, 64];
const PROBE_BYTES: usize = 64 * 64 * 3;
const NUT_MAGIC: &[u8] = b"nut/multimedia container\0";

pub(super) fn select_encoder(
  options: VideoOptions,
  accepted: &[VideoCodec],
  cancellation: &CaptureCancellation,
) -> io::Result<&'static str> {
  check_cancellation(cancellation.is_cancelled())?;
  options
    .validate()
    .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
  let preferences = match options.codec {
    VideoCodec::Auto | VideoCodec::Av1 => &[VideoCodec::Av1, VideoCodec::H264][..],
    VideoCodec::H264 => &[VideoCodec::H264][..],
  };
  let mut shared_codec = false;
  let frame = vec![0; PROBE_BYTES];
  for &codec in preferences.iter().filter(|codec| accepted.contains(codec)) {
    shared_codec = true;
    let names = match codec {
      VideoCodec::Av1 => AV1_ENCODERS,
      VideoCodec::H264 => H264_ENCODERS,
      VideoCodec::Auto => unreachable!("negotiation uses concrete codecs"),
    };
    for &name in names {
      if probe(
        encoder(options, name, PROBE_SIZE, PROBE_SIZE, PROBE_SIZE),
        &frame,
        || cancellation.is_cancelled(),
        |output| output.starts_with(NUT_MAGIC),
      )? {
        return Ok(name);
      }
    }
  }
  let message = if shared_codec {
    "FFmpeg has no usable encoder for the negotiated video codecs"
  } else {
    "the capture endpoint and viewer have no shared video codec"
  };
  Err(io::Error::new(io::ErrorKind::Unsupported, message))
}

pub(super) fn encoder_codec(name: &str) -> VideoCodec {
  if AV1_ENCODERS.contains(&name) {
    VideoCodec::Av1
  } else {
    debug_assert!(H264_ENCODERS.contains(&name));
    VideoCodec::H264
  }
}

fn ffmpeg() -> Command {
  let mut command = Command::new("ffmpeg");
  command.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
  command
}

pub(super) fn encoder(
  options: VideoOptions,
  name: &str,
  source: [usize; 2],
  coded: [usize; 2],
  display: [usize; 2],
) -> Command {
  let [source_width, source_height] = source;
  let [coded_width, coded_height] = coded;
  let [display_width, display_height] = display;
  let fps = options.max_fps.to_string();
  let bitrate = options.quality.bitrate();
  let mut command = ffmpeg();
  command.args([
    "-filter_threads",
    "2",
    "-f",
    "rawvideo",
    "-pixel_format",
    "rgb24",
    "-video_size",
    &format!("{source_width}x{source_height}"),
    "-framerate",
    &fps,
    "-probesize",
    "32",
    "-analyzeduration",
    "1",
    "-i",
    "pipe:0",
    "-map",
    "0:v:0",
  ]);
  if source != display || coded != display {
    let scale = format!("scale={display_width}:{display_height}:flags=fast_bilinear");
    let mut filters = vec![scale];
    if coded != display {
      // The border filter can fill up to half of each input dimension.
      let smear_width = coded_width.min(display_width * 2);
      let smear_height = coded_height.min(display_height * 2);
      let right = smear_width - display_width;
      let bottom = smear_height - display_height;
      // Preserve odd edges before 4:2:0 subsampling. Repeat the edge into the
      // padding so the last displayed chroma sample keeps its color.
      filters.push(format!(
        "format=gbrp,pad={smear_width}:{smear_height}:0:0,fillborders=right={right}:bottom={bottom}:mode=smear"
      ));
      if [smear_width, smear_height] != coded {
        filters.push(format!("pad={coded_width}:{coded_height}:0:0"));
      }
    }
    command.arg("-vf").arg(filters.join(","));
  }
  command.args([
    "-c:v",
    name,
    "-pix_fmt",
    "yuv420p",
    "-threads",
    "4",
    "-b:v",
    &bitrate.to_string(),
    "-bufsize",
    &(bitrate / 2).to_string(),
    "-g",
    &fps,
  ]);
  match name {
    // SVT's low-delay prediction structure requires CBR. Its maxrate setting
    // applies to the random-access mode, so set only the target bitrate here.
    "libsvtav1" => command.args([
      "-preset",
      "12",
      "-svtav1-params",
      "pred-struct=1:rc=2:lp=2:lookahead=0",
    ]),
    "libaom-av1" => command.args([
      "-usage",
      "realtime",
      "-cpu-used",
      "8",
      "-lag-in-frames",
      "0",
      "-row-mt",
      "1",
      "-maxrate",
      &bitrate.to_string(),
    ]),
    "libx264" => command.args([
      "-preset",
      "ultrafast",
      "-tune",
      "zerolatency",
      "-maxrate",
      &bitrate.to_string(),
    ]),
    "h264_videotoolbox" => command.args(["-realtime", "1", "-bf", "0"]),
    _ => unreachable!("unknown negotiated FFmpeg encoder"),
  };
  command.args([
    "-f",
    "nut",
    "-write_index",
    "0",
    "-flush_packets",
    "1",
    "pipe:1",
  ]);
  command
}

pub(super) fn decoder() -> Command {
  let mut command = ffmpeg();
  command.args([
    "-threads",
    "1",
    "-f",
    "nut",
    "-probesize",
    "32",
    "-analyzeduration",
    "1",
    "-max_pixels",
    "33554432",
    "-codec_whitelist",
    "av1,libdav1d,libaom-av1,h264",
    "-i",
    "pipe:0",
    "-map",
    "0:v:0",
    "-c:v",
    "ppm",
    "-pix_fmt",
    "rgb24",
    "-f",
    "image2pipe",
    "-flush_packets",
    "1",
    "pipe:1",
  ]);
  command
}

pub(super) fn available_decoders() -> io::Result<Vec<VideoCodec>> {
  let mut codecs = Vec::new();
  for (codec, sample) in [
    (VideoCodec::Av1, AV1_SAMPLE),
    (VideoCodec::H264, H264_SAMPLE),
  ] {
    if probe(
      decoder(),
      sample,
      || false,
      |output| {
        output
          .strip_prefix(b"P6\n64 64\n255\n")
          .is_some_and(|pixels| pixels.len() == PROBE_BYTES)
      },
    )? {
      codecs.push(codec);
    }
  }
  Ok(codecs)
}

fn probe(
  mut command: Command,
  input: &[u8],
  cancelled: impl Fn() -> bool,
  matches: impl FnOnce(&[u8]) -> bool,
) -> io::Result<bool> {
  const OUTPUT_LIMIT: u64 = 64 * 1024;
  check_cancellation(cancelled())?;
  let deadline = Instant::now() + Duration::from_secs(5);
  command.args(["-abort_on", "empty_output"]);
  let (child, mut input_pipe, output, stderr) = process::Process::start(command)?;
  // A stalled candidate should not prevent probing the next provider.
  thread::scope(|scope| {
    let written = scope.spawn(move || input_pipe.write_all(input));
    let bytes = scope.spawn(|| {
      let mut bytes = Vec::new();
      let count = output.take(OUTPUT_LIMIT + 1).read_to_end(&mut bytes)?;
      Ok::<_, io::Error>((count, bytes))
    });
    let diagnostics = scope.spawn(|| process::diagnostics(stderr));
    let status = child.wait(|| cancelled() || Instant::now() >= deadline);
    let written = process::join(written)?;
    let (count, bytes) = process::join(bytes)??;
    process::join(diagnostics)?;
    check_cancellation(cancelled())?;
    if !status?.success() || count as u64 > OUTPUT_LIMIT {
      return Ok(false);
    }
    written?;
    Ok(matches(&bytes))
  })
}

fn check_cancellation(cancelled: bool) -> io::Result<()> {
  if cancelled {
    return Err(io::Error::new(
      io::ErrorKind::Interrupted,
      "video encoder selection cancelled",
    ));
  }
  Ok(())
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use std::cell::Cell;

  #[test]
  fn cancelling_a_probe_stops_the_process_before_its_timeout() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exec sleep 30"]);
    let polled = Cell::new(false);
    let start = Instant::now();
    let error = probe(command, &[], || polled.replace(true), |_| false).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(start.elapsed() < Duration::from_secs(3));
  }
}

// One synthetic 64 × 64 black keyframe in each codec's NUT container. The
// viewer can verify decoding even when its FFmpeg build has no video encoders.
const AV1_SAMPLE: &[u8] = &[
  0x6e, 0x75, 0x74, 0x2f, 0x6d, 0x75, 0x6c, 0x74, 0x69, 0x6d, 0x65, 0x64, 0x69, 0x61, 0x20, 0x63,
  0x6f, 0x6e, 0x74, 0x61, 0x69, 0x6e, 0x65, 0x72, 0x00, 0x4e, 0x4d, 0x7a, 0x56, 0x1f, 0x5f, 0x04,
  0xad, 0x4a, 0x03, 0x01, 0x81, 0xff, 0x7f, 0x01, 0x01, 0x83, 0xe0, 0x00, 0xc0, 0x00, 0x06, 0x00,
  0x00, 0x00, 0x00, 0x00, 0x01, 0xa0, 0x00, 0x02, 0x01, 0x01, 0x28, 0x01, 0x00, 0x29, 0x00, 0x21,
  0x01, 0x9f, 0x7f, 0x20, 0x02, 0x9f, 0x7f, 0x81, 0x79, 0xc0, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00,
  0x00, 0x01, 0x06, 0x03, 0x00, 0x00, 0x01, 0x04, 0x00, 0x00, 0x01, 0xb6, 0x02, 0xff, 0xfa, 0x02,
  0xff, 0xfb, 0x02, 0xff, 0xfc, 0x02, 0xff, 0xfd, 0xb7, 0x41, 0xd9, 0xbe, 0x4e, 0x53, 0x11, 0x40,
  0x5b, 0xf2, 0xf9, 0xdb, 0x24, 0x00, 0x00, 0x04, 0x41, 0x56, 0x30, 0x31, 0x00, 0x0e, 0x83, 0xe0,
  0x00, 0x00, 0x00, 0x0c, 0x0a, 0x0a, 0x00, 0x00, 0x00, 0x02, 0xaf, 0xff, 0x80, 0x5f, 0x00, 0x08,
  0x40, 0x40, 0x01, 0x01, 0x00, 0x9c, 0x6e, 0xbf, 0x77, 0x4e, 0x49, 0xab, 0x68, 0xb5, 0x96, 0xba,
  0x78, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4e, 0x49, 0xab, 0x68, 0xb5,
  0x96, 0xba, 0x78, 0x34, 0x01, 0x00, 0x00, 0x00, 0x02, 0x07, 0x65, 0x6e, 0x63, 0x6f, 0x64, 0x65,
  0x72, 0x02, 0x0e, 0x4c, 0x61, 0x76, 0x63, 0x20, 0x6c, 0x69, 0x62, 0x73, 0x76, 0x74, 0x61, 0x76,
  0x31, 0x0c, 0x72, 0x5f, 0x66, 0x72, 0x61, 0x6d, 0x65, 0x5f, 0x72, 0x61, 0x74, 0x65, 0x02, 0x04,
  0x33, 0x30, 0x2f, 0x31, 0x49, 0xb3, 0x88, 0x7c, 0x4e, 0x4b, 0xe4, 0xad, 0xee, 0xca, 0x45, 0x69,
  0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x1e, 0x12, 0x00, 0x0a, 0x0a, 0x00, 0x00,
  0x00, 0x02, 0xaf, 0xff, 0x89, 0x5f, 0x20, 0x08, 0x32, 0x0e, 0x10, 0x00, 0x87, 0x00, 0x00, 0x00,
  0x00, 0x00, 0x00, 0x00, 0x1a, 0xa9, 0xce, 0xac,
];

const H264_SAMPLE: &[u8] = &[
  0x6e, 0x75, 0x74, 0x2f, 0x6d, 0x75, 0x6c, 0x74, 0x69, 0x6d, 0x65, 0x64, 0x69, 0x61, 0x20, 0x63,
  0x6f, 0x6e, 0x74, 0x61, 0x69, 0x6e, 0x65, 0x72, 0x00, 0x4e, 0x4d, 0x7a, 0x56, 0x1f, 0x5f, 0x04,
  0xad, 0x4a, 0x03, 0x01, 0x81, 0xff, 0x7f, 0x01, 0x01, 0x83, 0xe0, 0x00, 0xc0, 0x00, 0x06, 0x00,
  0x00, 0x00, 0x00, 0x00, 0x01, 0xa0, 0x00, 0x02, 0x01, 0x01, 0x28, 0x01, 0x00, 0x29, 0x00, 0x21,
  0x01, 0x9f, 0x7f, 0x20, 0x02, 0x9f, 0x7f, 0x81, 0x79, 0xc0, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00,
  0x00, 0x01, 0x06, 0x03, 0x00, 0x00, 0x01, 0x04, 0x00, 0x00, 0x01, 0xb6, 0x02, 0xff, 0xfa, 0x02,
  0xff, 0xfb, 0x02, 0xff, 0xfc, 0x02, 0xff, 0xfd, 0xb7, 0x41, 0xd9, 0xbe, 0x4e, 0x53, 0x11, 0x40,
  0x5b, 0xf2, 0xf9, 0xdb, 0x3a, 0x00, 0x00, 0x04, 0x48, 0x32, 0x36, 0x34, 0x00, 0x0e, 0x83, 0xe0,
  0x00, 0x00, 0x00, 0x22, 0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x0a, 0xda, 0x10, 0x9b, 0x01,
  0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xc8, 0xf1, 0x22, 0x6a, 0x00, 0x00,
  0x00, 0x01, 0x68, 0xce, 0x0f, 0xc8, 0x40, 0x40, 0x01, 0x01, 0x00, 0xab, 0x83, 0x26, 0xff, 0x4e,
  0x49, 0xab, 0x68, 0xb5, 0x96, 0xba, 0x78, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
  0x00, 0x4e, 0x49, 0xab, 0x68, 0xb5, 0x96, 0xba, 0x78, 0x32, 0x01, 0x00, 0x00, 0x00, 0x02, 0x07,
  0x65, 0x6e, 0x63, 0x6f, 0x64, 0x65, 0x72, 0x02, 0x0c, 0x4c, 0x61, 0x76, 0x63, 0x20, 0x6c, 0x69,
  0x62, 0x78, 0x32, 0x36, 0x34, 0x0c, 0x72, 0x5f, 0x66, 0x72, 0x61, 0x6d, 0x65, 0x5f, 0x72, 0x61,
  0x74, 0x65, 0x02, 0x04, 0x33, 0x30, 0x2f, 0x31, 0x3c, 0x00, 0xf6, 0x79, 0x4e, 0x4b, 0xe4, 0xad,
  0xee, 0xca, 0x45, 0x69, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x1a, 0x00, 0x00,
  0x00, 0x01, 0x65, 0x88, 0x84, 0x3a, 0x26, 0x28, 0x00, 0x09, 0x02, 0xc9, 0xc9, 0xc9, 0xd7, 0x5d,
  0x75, 0xd7, 0x5d, 0x75, 0xd7, 0x5d, 0x75, 0xe0,
];
