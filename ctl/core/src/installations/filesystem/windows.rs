use super::{
  Installations, LAUNCHER, ensure_directory, executable_bytes, invalid, path_exists,
  replace_launcher,
};
use crate::bundle_layout::{ALL, executable_name};
use std::io;

pub(super) fn prepare_aliases(store: &Installations, updater: Option<&[u8]>) -> io::Result<()> {
  let bin = store.root.join("bin");
  ensure_directory(&bin)?;
  let launcher = bin.join(LAUNCHER);
  let exists = path_exists(&launcher)?;
  let bytes;
  let source = if exists {
    bytes = executable_bytes(&launcher)?;
    Some(bytes.as_slice())
  } else {
    updater
  };
  let mut missing = Vec::new();
  for name in ALL.into_iter().chain(["ctlup"]) {
    let alias = bin.join(executable_name(crate::paths::native_target(), name).as_ref());
    if path_exists(&alias)? {
      if let Some(source) = source
        && executable_bytes(&alias)? == source
      {
        continue;
      }
      return Err(invalid(
        "stable command path already belongs to another installation",
      ));
    }
    missing.push(alias);
  }
  let Some(source) = source else {
    return Ok(());
  };
  if !exists {
    replace_launcher(&launcher, source)?;
  }
  for alias in missing {
    replace_launcher(&alias, source)?;
  }
  Ok(())
}
