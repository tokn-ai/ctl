use std::io;

#[derive(Clone, Copy)]
pub(super) enum Rotation {
  Identity,
  Clockwise90,
  Clockwise180,
  Clockwise270,
}

pub(super) fn output_size(size: [usize; 2], rotation: Rotation) -> io::Result<[usize; 2]> {
  let [width, height] = size;
  if !(1..=16384).contains(&width)
    || !(1..=16384).contains(&height)
    || width * height > 32 * 1024 * 1024
  {
    return Err(io::Error::other("invalid Windows capture dimensions"));
  }
  Ok(match rotation {
    Rotation::Identity | Rotation::Clockwise180 => size,
    Rotation::Clockwise90 | Rotation::Clockwise270 => [height, width],
  })
}

pub(super) fn mapped_length(size: [usize; 2], row_pitch: usize) -> io::Result<usize> {
  let [width, height] = output_size(size, Rotation::Identity)?;
  let needed = (height - 1)
    .checked_mul(row_pitch)
    .and_then(|offset| offset.checked_add(width * 4))
    .filter(|length| *length <= isize::MAX as usize);
  if row_pitch < width * 4 {
    return Err(io::Error::other("invalid Windows capture row layout"));
  }
  needed.ok_or_else(|| io::Error::other("invalid Windows capture memory layout"))
}

/// A WGC texture may straddle a pool resize. Only the content rectangle is
/// initialized; a texture smaller in either axis cannot provide a complete frame.
pub(super) fn window_content_size(
  texture: [usize; 2],
  content: [usize; 2],
) -> io::Result<Option<[usize; 2]>> {
  output_size(texture, Rotation::Identity)?;
  output_size(content, Rotation::Identity)?;
  Ok((texture[0] >= content[0] && texture[1] >= content[1]).then_some(content))
}

/// Convert initialized pixel rows, without reading the GPU's undefined row padding.
pub(super) fn convert_rows(
  rows: &[&[u8]],
  size: [usize; 2],
  rotation: Rotation,
  rgb: &mut Vec<u8>,
) -> io::Result<[usize; 2]> {
  let [width, height] = size;
  let [out_width, out_height] = output_size(size, rotation)?;
  if rows.len() != height || rows.iter().any(|row| row.len() < width * 4) {
    return Err(io::Error::other("invalid Windows capture row layout"));
  }
  rgb.resize(width * height * 3, 0);
  for y in 0..out_height {
    for x in 0..out_width {
      let (source_x, source_y) = match rotation {
        Rotation::Identity => (x, y),
        Rotation::Clockwise90 => (y, height - 1 - x),
        Rotation::Clockwise180 => (width - 1 - x, height - 1 - y),
        Rotation::Clockwise270 => (width - 1 - y, x),
      };
      let source = rows[source_y];
      let from = source_x * 4;
      let to = (y * out_width + x) * 3;
      rgb[to..to + 3].copy_from_slice(&[source[from + 2], source[from + 1], source[from]]);
    }
  }
  Ok([out_width, out_height])
}

#[cfg(test)]
mod tests {
  use super::*;

  fn convert_bgra(
    source: &[u8],
    size: [usize; 2],
    row_pitch: usize,
    rotation: Rotation,
    rgb: &mut Vec<u8>,
  ) -> io::Result<[usize; 2]> {
    if source.len() < mapped_length(size, row_pitch)? {
      return Err(io::Error::other("incomplete capture rows"));
    }
    let rows: Vec<_> = (0..size[1])
      .map(|y| &source[y * row_pitch..y * row_pitch + size[0] * 4])
      .collect();
    convert_rows(&rows, size, rotation, rgb)
  }

  fn source() -> Vec<u8> {
    // Two padded rows: A B C / D E F. Every channel differs, including alpha.
    let mut source = Vec::new();
    for row in [[1_u8, 2, 3], [4, 5, 6]] {
      for value in row {
        source.extend([value + 20, value + 10, value, 0x80]);
      }
      source.extend([0xee; 4]);
    }
    source
  }

  #[test]
  fn window_resize_reads_only_complete_initialized_content() {
    // Old buffers arriving after growth must not yield a clipped frame.
    assert_eq!(window_content_size([404, 292], [624, 432]).unwrap(), None);
    assert_eq!(window_content_size([624, 252], [344, 432]).unwrap(), None);
    assert_eq!(window_content_size([344, 432], [624, 252]).unwrap(), None);
    // Shrink can retain a larger texture, but bytes outside content are undefined.
    assert_eq!(
      window_content_size([624, 432], [344, 252]).unwrap(),
      Some([344, 252])
    );
    assert_eq!(
      window_content_size([624, 432], [624, 432]).unwrap(),
      Some([624, 432])
    );
    assert!(window_content_size([0, 432], [344, 252]).is_err());
    assert!(window_content_size([624, 432], [0, 252]).is_err());
    assert!(window_content_size([usize::MAX, 432], [344, 252]).is_err());
  }

  #[test]
  fn padded_bgra_rows_rotate_into_upright_rgb() {
    for (rotation, size, order) in [
      (Rotation::Identity, [3, 2], [1, 2, 3, 4, 5, 6]),
      (Rotation::Clockwise90, [2, 3], [4, 1, 5, 2, 6, 3]),
      (Rotation::Clockwise180, [3, 2], [6, 5, 4, 3, 2, 1]),
      (Rotation::Clockwise270, [2, 3], [3, 6, 2, 5, 1, 4]),
    ] {
      let mut rgb = vec![0xff; 100];
      assert_eq!(
        convert_bgra(&source(), [3, 2], 16, rotation, &mut rgb).unwrap(),
        size
      );
      let expected: Vec<u8> = order
        .into_iter()
        .flat_map(|value| [value, value + 10, value + 20])
        .collect();
      assert_eq!(rgb, expected);
    }
  }

  #[test]
  fn invalid_dimensions_strides_and_short_rows_are_rejected() {
    for (size, pitch, length) in [
      ([0, 1], 4, 4),
      ([1, 0], 4, 4),
      ([usize::MAX, 2], 4, 4),
      ([16384, 16384], 65536, 4),
      ([3, 2], 11, 32),
      ([3, 2], 16, 27),
      ([1, 2], usize::MAX, 4),
    ] {
      assert!(
        convert_bgra(
          &vec![0; length],
          size,
          pitch,
          Rotation::Identity,
          &mut Vec::new()
        )
        .is_err()
      );
    }
    // The final row need not include its padding.
    assert!(
      convert_bgra(
        &source()[..28],
        [3, 2],
        16,
        Rotation::Identity,
        &mut Vec::new()
      )
      .is_ok()
    );
    assert!(convert_rows(&[&[0; 3]], [1, 1], Rotation::Identity, &mut Vec::new()).is_err());
    assert!(convert_rows(&[], [1, 1], Rotation::Identity, &mut Vec::new()).is_err());
  }
}
