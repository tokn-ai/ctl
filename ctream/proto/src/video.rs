use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const MAX_VIDEO_CHUNK: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
  Auto,
  #[default]
  Av1,
  H264,
}

impl VideoCodec {
  pub const ALL: [Self; 3] = [Self::Auto, Self::Av1, Self::H264];

  pub fn parse(value: &str) -> Result<Self, &'static str> {
    match value {
      "auto" => Ok(Self::Auto),
      "av1" => Ok(Self::Av1),
      "h264" => Ok(Self::H264),
      _ => Err("video codec must be auto, av1, or h264"),
    }
  }

  pub const fn label(self) -> &'static str {
    match self {
      Self::Auto => "Auto",
      Self::Av1 => "AV1",
      Self::H264 => "H.264",
    }
  }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VideoQuality {
  Low,
  #[default]
  Balanced,
  High,
}

impl VideoQuality {
  pub const ALL: [Self; 3] = [Self::Low, Self::Balanced, Self::High];

  pub fn parse(value: &str) -> Result<Self, &'static str> {
    match value {
      "low" => Ok(Self::Low),
      "balanced" => Ok(Self::Balanced),
      "high" => Ok(Self::High),
      _ => Err("video quality must be low, balanced, or high"),
    }
  }

  pub const fn label(self) -> &'static str {
    match self {
      Self::Low => "Low bandwidth",
      Self::Balanced => "Balanced",
      Self::High => "High quality",
    }
  }

  pub const fn max_dimension(self) -> usize {
    match self {
      Self::Low => 1280,
      Self::Balanced => 1920,
      Self::High => 3840,
    }
  }

  pub const fn bitrate(self) -> u32 {
    match self {
      Self::Low => 1_500_000,
      Self::Balanced => 4_000_000,
      Self::High => 12_000_000,
    }
  }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct VideoOptions {
  pub codec: VideoCodec,
  pub quality: VideoQuality,
  pub max_fps: u16,
}

impl Default for VideoOptions {
  fn default() -> Self {
    Self {
      codec: VideoCodec::Av1,
      quality: VideoQuality::Balanced,
      max_fps: 30,
    }
  }
}

impl VideoOptions {
  pub fn validate(self) -> Result<(), &'static str> {
    if !(1..=120).contains(&self.max_fps) {
      return Err("video frame rate must be between 1 and 120 FPS");
    }
    Ok(())
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VideoCapabilities {
  pub codecs: Vec<VideoCodec>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VideoRequest {
  pub options: VideoOptions,
  pub codecs: Vec<VideoCodec>,
}

impl VideoRequest {
  pub fn validate(&self) -> Result<(), &'static str> {
    self.options.validate()?;
    if self.codecs.is_empty()
      || self.codecs.len() > 2
      || self.codecs.contains(&VideoCodec::Auto)
      || (self.codecs.len() == 2 && self.codecs[0] == self.codecs[1])
      || (self.options.codec == VideoCodec::H264 && self.codecs != [VideoCodec::H264])
    {
      return Err("invalid accepted video codecs");
    }
    Ok(())
  }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct VideoInfo {
  pub codec: VideoCodec,
  pub encoder: String,
  pub quality: VideoQuality,
  pub max_fps: u16,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VideoStarted {
  pub driver: String,
  pub video: VideoInfo,
}

/// Each geometry segment carries a fresh NUT stream and its display crop.
#[derive(Debug, PartialEq, Eq)]
pub enum VideoPacket {
  Segment { size: [usize; 2] },
  Data(Vec<u8>),
  End,
}

pub fn pixel_count([width, height]: [usize; 2]) -> io::Result<usize> {
  if !(1..=16384).contains(&width)
    || !(1..=16384).contains(&height)
    || width * height > 32 * 1024 * 1024
  {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid video frame size",
    ));
  }
  Ok(width * height)
}

pub fn write_segment(output: &mut impl Write, size: [usize; 2]) -> io::Result<()> {
  pixel_count(size)?;
  output.write_all(&[1])?;
  for dimension in size {
    output.write_all(&(dimension as u32).to_le_bytes())?;
  }
  output.flush()
}

pub fn write_data(output: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
  if bytes.is_empty() || bytes.len() > MAX_VIDEO_CHUNK {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid video chunk size",
    ));
  }
  output.write_all(&[2])?;
  output.write_all(&(bytes.len() as u32).to_le_bytes())?;
  output.write_all(bytes)?;
  output.flush()
}

pub fn write_end(output: &mut impl Write) -> io::Result<()> {
  output.write_all(&[3])?;
  output.flush()
}

pub fn read_packet(input: &mut impl Read) -> io::Result<Option<VideoPacket>> {
  let mut tag = [0];
  loop {
    match input.read(&mut tag) {
      Ok(0) => return Ok(None),
      Ok(_) => break,
      Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
      Err(error) => return Err(error),
    }
  }
  let mut number = || {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    Ok::<_, io::Error>(u32::from_le_bytes(bytes) as usize)
  };
  let packet = match tag[0] {
    1 => {
      let size = [number()?, number()?];
      pixel_count(size)?;
      VideoPacket::Segment { size }
    }
    2 => {
      let size = number()?;
      if size == 0 || size > MAX_VIDEO_CHUNK {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "invalid video chunk size",
        ));
      }
      let mut bytes = vec![0; size];
      input.read_exact(&mut bytes)?;
      VideoPacket::Data(bytes)
    }
    3 => VideoPacket::End,
    _ => {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "unknown video packet",
      ));
    }
  };
  Ok(Some(packet))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn av1_defaults_allow_h264_fallback_and_reject_invalid_requests() {
    let options = VideoOptions::default();
    assert_eq!(options.codec, VideoCodec::Av1);
    VideoRequest {
      options,
      codecs: vec![VideoCodec::H264],
    }
    .validate()
    .unwrap();
    for codecs in [
      vec![],
      vec![VideoCodec::Auto],
      vec![VideoCodec::Av1, VideoCodec::Av1],
    ] {
      assert!(VideoRequest { options, codecs }.validate().is_err());
    }
    assert!(
      VideoRequest {
        options: VideoOptions {
          codec: VideoCodec::H264,
          ..options
        },
        codecs: vec![VideoCodec::Av1, VideoCodec::H264],
      }
      .validate()
      .is_err()
    );
    assert!(
      VideoOptions {
        max_fps: 0,
        ..options
      }
      .validate()
      .is_err()
    );
    assert!(
      VideoOptions {
        max_fps: 121,
        ..options
      }
      .validate()
      .is_err()
    );
  }

  #[test]
  fn bounded_packets_preserve_resize_crop_and_encoded_bytes() {
    let mut bytes = Vec::new();
    write_segment(&mut bytes, [65, 49]).unwrap();
    write_data(&mut bytes, b"encoded bytes").unwrap();
    write_segment(&mut bytes, [83, 67]).unwrap();
    write_end(&mut bytes).unwrap();
    let mut reader = bytes.as_slice();
    assert_eq!(
      read_packet(&mut reader).unwrap(),
      Some(VideoPacket::Segment { size: [65, 49] })
    );
    assert_eq!(
      read_packet(&mut reader).unwrap(),
      Some(VideoPacket::Data(b"encoded bytes".to_vec()))
    );
    assert_eq!(
      read_packet(&mut reader).unwrap(),
      Some(VideoPacket::Segment { size: [83, 67] })
    );
    assert_eq!(read_packet(&mut reader).unwrap(), Some(VideoPacket::End));
    assert_eq!(read_packet(&mut reader).unwrap(), None);
  }

  #[test]
  fn malformed_video_sizes_and_truncated_payloads_fail_before_decode() {
    let mut oversized = vec![2];
    oversized.extend_from_slice(&((MAX_VIDEO_CHUNK + 1) as u32).to_le_bytes());
    assert_eq!(
      read_packet(&mut oversized.as_slice()).unwrap_err().kind(),
      io::ErrorKind::InvalidData
    );
    assert_eq!(
      read_packet(&mut &[2, 2, 0, 0, 0, 1][..])
        .unwrap_err()
        .kind(),
      io::ErrorKind::UnexpectedEof
    );
    assert!(write_segment(&mut Vec::new(), [0, 100]).is_err());
    assert!(write_segment(&mut Vec::new(), [16384, 16384]).is_err());
    assert!(read_packet(&mut &[99][..]).is_err());
  }
}
