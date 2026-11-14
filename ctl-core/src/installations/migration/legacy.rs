//! Old storage is read only during one-time adoption.

use super::{Bundle, Installations, filesystem};
#[cfg(unix)]
use crate::bundles::{MANIFEST_FILE, Purpose, Store};
use std::io;
#[cfg(unix)]
use std::path::{Component, PathBuf};

pub(super) fn managed_bundles(store: &Installations) -> io::Result<Vec<Bundle>> {
  filesystem::entries(store)?
    .into_iter()
    .map(|(id, path)| {
      let bundle = Bundle::open(&path)?;
      if bundle.manifest.bundle_id != id {
        return Err(filesystem::invalid(
          "installation payload identity differs from its path",
        ));
      }
      Ok(bundle)
    })
    .collect()
}

#[cfg(unix)]
pub(super) fn require_recoverable_activation(store: &Installations) -> io::Result<()> {
  use std::io::Read as _;
  #[derive(serde::Deserialize)]
  #[serde(deny_unknown_fields)]
  struct PendingActivation {
    active: String,
    previous: Option<String>,
  }
  let path = store.root.join(".activation.json");
  if !filesystem::path_exists(&path)? {
    return Ok(());
  }
  filesystem::private_file(&std::fs::symlink_metadata(&path)?)?;
  let file = crate::private_fs::options().read(true).open(path)?;
  filesystem::private_file(&file.metadata()?)?;
  let mut bytes = Vec::new();
  file.take(1025).read_to_end(&mut bytes)?;
  if bytes.len() > 1024 {
    return Err(filesystem::invalid("pending activation is oversized"));
  }
  let pending: PendingActivation = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  let state = super::catalog::read_for_migration(store)?;
  if [Some(pending.active.as_str()), pending.previous.as_deref()]
    .into_iter()
    .flatten()
    .any(|id| {
      !state
        .entries
        .iter()
        .any(|entry| entry.bundle_id == id && crate::bundles::local_target(&entry.target_triple))
    })
  {
    return Err(filesystem::invalid(
      "pending activation has no catalog record; complete recovery using the previous manager before migrating, or perform a clean reinstall",
    ));
  }
  Ok(())
}

#[cfg(windows)]
pub(super) fn active(store: &Installations) -> io::Result<Option<Bundle>> {
  filesystem::pointer(store, "current")
}

#[cfg(windows)]
pub(super) fn previous(store: &Installations) -> io::Result<Option<Bundle>> {
  filesystem::pointer(store, "previous")
}

#[cfg(unix)]
pub(super) fn active(store: &Installations) -> io::Result<Option<Bundle>> {
  if let Some(bundle) = pointer(store, "current")? {
    return Ok(Some(bundle));
  }
  if filesystem::path_exists(&store.root.join("current"))? {
    return Ok(None);
  }
  Store::from_data_root(&store.root).selected(Purpose::Local, crate::paths::native_target())
}

#[cfg(unix)]
pub(super) fn previous(store: &Installations) -> io::Result<Option<Bundle>> {
  pointer(store, "previous")
}

#[cfg(unix)]
fn pointer(store: &Installations, name: &str) -> io::Result<Option<Bundle>> {
  let Some(path) = pointer_path(store, name)? else {
    return Ok(None);
  };
  if path.starts_with(store.root.join("components/agents"))
    || (path.starts_with(store.root.join("versions"))
      && !filesystem::path_exists(&path.join(MANIFEST_FILE))?)
  {
    return Ok(None);
  }
  let bundle = Bundle::open(&path)?;
  if !crate::bundles::local_target(&bundle.manifest.target_triple) {
    return Err(filesystem::invalid(
      "an installation must match this computer's target",
    ));
  }
  if path.starts_with(store.root.join("installs"))
    && path.file_name().and_then(|name| name.to_str()) != Some(&bundle.manifest.bundle_id)
  {
    return Err(filesystem::invalid(
      "installation payload identity differs from its path",
    ));
  }
  Ok(Some(bundle))
}

#[cfg(unix)]
fn pointer_path(store: &Installations, name: &str) -> io::Result<Option<PathBuf>> {
  let path = store.root.join(name);
  if !filesystem::path_exists(&path)? {
    return Ok(None);
  }
  let link = std::fs::read_link(path)?;
  let absolute = link.is_absolute();
  let link = if absolute {
    let physical = std::fs::canonicalize(&store.root)?;
    link
      .strip_prefix(&physical)
      .or_else(|_| link.strip_prefix(&store.root))
      .map_err(|_| {
        filesystem::invalid("absolute installation pointer is outside legacy agent storage")
      })?
      .to_owned()
  } else {
    link
  };
  if link
    .components()
    .any(|part| !matches!(part, Component::Normal(_)))
  {
    return Err(filesystem::invalid(
      "installation pointer must be a managed relative link",
    ));
  }
  let parts = link.iter().collect::<Vec<_>>();
  if absolute {
    let agent = name == "current"
      && parts.len() == 4
      && parts[0] == "components"
      && parts[1] == "agents"
      && parts[2]
        .to_str()
        .is_some_and(|target| crate::bundles::validate_target(target).is_ok())
      && parts[3].to_str().is_some_and(|id| {
        id.split_once('-').is_some_and(|(id, suffix)| {
          filesystem::valid_id(id)
            && suffix.len() == 6
            && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
      });
    if !agent {
      return Err(filesystem::invalid(
        "absolute installation pointer is outside legacy agent storage",
      ));
    }
  }
  let managed = parts.len() == 2
    && parts[0] == "installs"
    && parts[1].to_str().is_some_and(filesystem::valid_id);
  let old = name == "current"
    && ((parts.len() == 4 && parts[0] == "components" && parts[1] == "bundles")
      || (parts.len() == 4 && parts[0] == "components" && parts[1] == "agents")
      || (parts.len() == 2 && parts[0] == "versions"));
  if !managed && !old {
    return Err(filesystem::invalid(
      "installation pointer is outside managed payload storage",
    ));
  }
  let mut parent = store.root.clone();
  for part in parts {
    parent.push(part);
    filesystem::directory(&parent)?;
  }
  Ok(Some(parent))
}
