use ctl_core::private_fs;
use lnks::Shortcut;
use std::{fs, io, path::Path};

// Explorer follows the native shortcut straight into the installed GUI.
pub(super) fn create(destination: &Path, target: &Path) -> io::Result<()> {
  if !private_fs::is_owned_file(&fs::symlink_metadata(target)?) {
    return Err(io::Error::other("desktop executable is not an owned file"));
  }
  let destination = std::path::absolute(destination)?;
  let target = dunce::canonicalize(target)?;
  let mut shortcut = Shortcut::new(&target);
  shortcut.working_dir = target.parent().map(Path::to_path_buf);
  shortcut.save(&destination).map_err(io::Error::other)?;
  if !private_fs::is_owned_file(&fs::symlink_metadata(&destination)?) {
    return Err(io::Error::other("desktop shortcut is not an owned file"));
  }
  Ok(())
}

pub(super) fn managed(path: &Path, store: &Path) -> io::Result<bool> {
  let shortcut = Shortcut::load(path).map_err(io::Error::other)?;
  let Some(target) = shortcut.target_path.as_deref() else {
    return Ok(false);
  };
  if shortcut.arguments.is_some() || shortcut.working_dir.as_deref() != target.parent() {
    return Ok(false);
  }
  // Resolve the store, since the shell records a full path. The immutable
  // object itself may be missing when this link is inspected for repair.
  let store = dunce::canonicalize(store)?;
  let Ok(relative) = target.strip_prefix(store) else {
    return Ok(false);
  };
  let mut components = relative.components();
  let valid_digest = components
    .next()
    .and_then(|component| component.as_os_str().to_str())
    .is_some_and(|digest| super::Digest::parse(digest).is_ok());
  Ok(valid_digest && components.as_path() == Path::new(r"ctmux\ctmux-app.exe"))
}
