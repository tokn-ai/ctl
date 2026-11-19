use eframe::egui::ColorImage;
use std::io::{self, BufRead, Read};

use ctream::protocol::{Target, X11_FRAME_SIZE};

#[derive(Clone, Copy)]
pub enum FrameFormat {
  FixedRgb,
  Ppm,
}

impl FrameFormat {
  pub fn for_capture(target: Target, window: bool) -> Self {
    match (target, window) {
      (Target::X11(_), false) => Self::FixedRgb,
      _ => Self::Ppm,
    }
  }
}

// Window streams and Wayland display streams carry their dimensions in each frame.
fn header_line(reader: &mut impl BufRead) -> io::Result<String> {
  let mut line = String::new();
  reader.take(64).read_line(&mut line)?;
  if !line.ends_with('\n') {
    return Err(io::Error::other("missing or incomplete PPM frame header"));
  }
  Ok(line)
}

pub fn read_frame(
  reader: &mut impl BufRead,
  format: FrameFormat,
  pixels: &mut Vec<u8>,
) -> io::Result<ColorImage> {
  let size = match format {
    FrameFormat::FixedRgb => X11_FRAME_SIZE,
    FrameFormat::Ppm => {
      if header_line(reader)?.trim_end() != "P6" {
        return Err(io::Error::other("expected a P6 RGB frame"));
      }
      let dimensions = header_line(reader)?;
      let mut parts = dimensions.split_ascii_whitespace();
      let mut dimension = || {
        parts
          .next()
          .and_then(|s| s.parse::<usize>().ok())
          .filter(|n| (1..=16384).contains(n))
          .ok_or_else(|| io::Error::other("invalid PPM frame size"))
      };
      let size = [dimension()?, dimension()?];
      if parts.next().is_some()
        || size[0] * size[1] > 32 * 1024 * 1024
        || header_line(reader)?.trim_end() != "255"
      {
        return Err(io::Error::other("unsupported PPM frame"));
      }
      size
    }
  };
  pixels.resize(size[0] * size[1] * 3, 0);
  reader.read_exact(pixels)?;
  Ok(ColorImage::from_rgb(size, pixels))
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn successive_dimensioned_frames_can_resize() {
    for (target, window) in [(Target::Wayland(1), false), (Target::X11(1), true)] {
      let format = FrameFormat::for_capture(target, window);
      let mut data = &b"P6\n1 1\n255\n\x20\x0a\x00P6\n2 1\n255\nabcdef"[..];
      let mut rgb = Vec::new();
      for size in [[1, 1], [2, 1]] {
        assert_eq!(read_frame(&mut data, format, &mut rgb).unwrap().size, size);
      }
      assert!(read_frame(&mut data, format, &mut rgb).is_err());
    }
  }
}
