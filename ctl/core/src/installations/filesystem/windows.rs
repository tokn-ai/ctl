use super::{
  Installations, LAUNCHER, catalog, ensure_directory, executable_bytes, invalid, path_exists,
  read_manager, replace_launcher,
};
use crate::bundle_layout::{ALL, executable_name};
use std::io;

pub(super) fn prepare_aliases(
  store: &Installations,
  updater: Option<&[u8]>,
  refresh: bool,
) -> io::Result<()> {
  let bin = store.root.join("bin");
  ensure_directory(&bin)?;
  let launcher = bin.join(LAUNCHER);
  let exists = path_exists(&launcher)?;
  let bytes;
  let previous = if exists {
    bytes = executable_bytes(&launcher)?;
    Some(bytes.as_slice())
  } else {
    updater
  };
  let source = if refresh { updater } else { previous };
  let mut replacements = Vec::new();
  for name in ALL.into_iter().chain(["ctlup"]) {
    let alias = bin.join(executable_name(crate::paths::native_target(), name).as_ref());
    if path_exists(&alias)? {
      let bytes = executable_bytes(&alias)?;
      if source.is_some_and(|source| bytes == source) {
        continue;
      }
      // A failed layout transition can leave some aliases pointing at an
      // earlier verified updater. Replace only bytes this profile owns.
      if !refresh
        || (previous.is_none_or(|previous| bytes != previous) && !owns_updater(store, &bytes)?)
      {
        return Err(invalid(
          "stable command path already belongs to another installation",
        ));
      }
    }
    replacements.push(alias);
  }
  let Some(source) = source else {
    return Ok(());
  };
  for alias in replacements {
    // Rename can fail while Windows runs a previous command. Selection is
    // committed only after every alias succeeds. Retry uses verified copies.
    replace_launcher(&alias, source)?;
  }
  if !exists || (refresh && previous != Some(source)) {
    replace_launcher(&launcher, source)?;
  }
  Ok(())
}

fn owns_updater(store: &Installations, bytes: &[u8]) -> io::Result<bool> {
  let state = catalog::read_index(store)?;
  read_manager(store, &state.updater_paths, &crate::bundles::digest(bytes))
    .map(|payload| payload.is_some())
}
