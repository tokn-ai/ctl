use rustix::fs::{Mode, OFlags};
use sha2::{Digest as _, Sha256};
use std::fs::{self, File, Permissions};
use std::io::{self, Read as _, Seek as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

pub(super) fn verify_existing(path: &Path) -> io::Result<()> {
  open_image(path).map(|_| ())
}

pub(super) fn is_executable(path: &Path) -> io::Result<bool> {
  Ok(fs::symlink_metadata(path)?.permissions().mode() & 0o100 != 0)
}

pub(super) fn prepare(path: PathBuf) -> io::Result<PathBuf> {
  let image = open_image(&path)?;
  image.set_permissions(Permissions::from_mode(0o755))?;
  image.sync_all()?;
  Ok(path)
}

pub(super) fn matches(installed: &Path, staged: &Path) -> io::Result<bool> {
  let mut installed = open_image(installed)?;
  let mut staged = open_image(staged)?;
  let size = staged.metadata()?.len();
  if installed.metadata()?.len() != size || size > 512 * 1024 * 1024 {
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
  let mut header = [0; 11];
  if file.metadata()?.is_file()
    && file.read_exact(&mut header).is_ok()
    && header[..4] == *b"\x7fELF"
    && header[8..] == *b"AI\x02"
  {
    return Ok(file);
  }
  Err(io::Error::new(
    io::ErrorKind::InvalidData,
    format!("expected an AppImage at {}", path.display()),
  ))
}
