use eframe::egui::ColorImage;
use std::io::{self, BufRead, Read};

use ctream::protocol::{Target, X11_FRAME_SIZE};

// grim writes a P6 header for every frame, including after an output resize.
fn header_line(reader: &mut impl BufRead) -> io::Result<String> {
  let mut line = String::new();
  reader.take(64).read_line(&mut line)?;
  if !line.ends_with('\n') {
    return Err(io::Error::other("missing or incomplete grim frame header"));
  }
  Ok(line)
}

pub fn read_frame(
  reader: &mut impl BufRead,
  target: &Target,
  pixels: &mut Vec<u8>,
) -> io::Result<ColorImage> {
  let size = match target {
    Target::X11(_) => X11_FRAME_SIZE,
    Target::Wayland(_) => {
      if header_line(reader)?.trim_end() != "P6" {
        return Err(io::Error::other("expected RGB frame from grim"));
      }
      let dimensions = header_line(reader)?;
      let mut parts = dimensions.split_ascii_whitespace();
      let mut dimension = || {
        parts
          .next()
          .and_then(|s| s.parse::<usize>().ok())
          .filter(|n| (1..=16384).contains(n))
          .ok_or_else(|| io::Error::other("invalid grim frame size"))
      };
      let size = [dimension()?, dimension()?];
      if parts.next().is_some()
        || size[0] * size[1] > 32 * 1024 * 1024
        || header_line(reader)?.trim_end() != "255"
      {
        return Err(io::Error::other("unsupported grim frame"));
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
  fn successive_wayland_frames_can_resize() {
    let mut data = &b"P6\n1 1\n255\n\x20\x0a\x00P6\n2 1\n255\nabcdef"[..];
    let mut rgb = Vec::new();
    for size in [[1, 1], [2, 1]] {
      assert_eq!(
        read_frame(&mut data, &Target::Wayland(1), &mut rgb)
          .unwrap()
          .size,
        size
      );
    }
    assert!(read_frame(&mut data, &Target::Wayland(1), &mut rgb).is_err());
  }
}
