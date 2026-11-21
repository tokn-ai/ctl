use rustix::fs::{Mode, OFlags};
use sha2::{Digest as _, Sha256};
use std::fs::{File, Permissions};
use std::io::{self, Read as _, Seek as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

pub(super) fn prepare(path: PathBuf) -> io::Result<PathBuf> {
  let image = open_image(&path)?;
  image.set_permissions(Permissions::from_mode(0o755))?;
  image.sync_all()?;
  Ok(path)
}

pub(super) fn matches(installed: &Path, staged: &Path) -> io::Result<bool> {
  let mut installed = open_image(installed)?;
  let mut staged = open_image(staged)?;
  let installed_metadata = installed.metadata()?;
  let staged_metadata = staged.metadata()?;
  let size = staged_metadata.len();
  if installed_metadata.len() != size
    || installed_metadata.permissions().mode() & 0o100
      != staged_metadata.permissions().mode() & 0o100
    || size > 512 * 1024 * 1024
  {
    return Ok(false);
  }
  installed.rewind()?;
  staged.rewind()?;
  let mut expected = Sha256::new();
  let mut actual = Sha256::new();
  let expected_size = io::copy(&mut staged.take(size + 1), &mut expected)?;
  let actual_size = io::copy(&mut installed.take(size + 1), &mut actual)?;
  Ok(expected_size == size && actual_size == size && actual.finalize() == expected.finalize())
}

fn open_image(path: &Path) -> io::Result<File> {
  let mut file = File::from(rustix::fs::open(
    path,
    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
    Mode::empty(),
  )?);
  // AppImage type 2 uses a native 64-bit little-endian ELF executable.
  let mut header = [0; 20];
  let machine = if cfg!(target_arch = "aarch64") {
    183
  } else {
    62
  };
  if file.metadata()?.is_file()
    && file.read_exact(&mut header).is_ok()
    && header[..6] == *b"\x7fELF\x02\x01"
    && header[8..11] == *b"AI\x02"
    && u16::from_le_bytes([header[18], header[19]]) == machine
  {
    return Ok(file);
  }
  Err(io::Error::new(
    io::ErrorKind::InvalidData,
    format!("expected a native AppImage at {}", path.display()),
  ))
}

#[cfg(test)]
pub(super) fn fixture_bytes() -> Vec<u8> {
  let mut bytes = vec![0; 20];
  bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
  bytes[8..11].copy_from_slice(b"AI\x02");
  bytes[18] = if cfg!(target_arch = "aarch64") {
    183
  } else {
    62
  };
  bytes.extend_from_slice(b"fixture");
  bytes
}

#[cfg(test)]
mod tests;
