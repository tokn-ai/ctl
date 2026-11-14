//! Readable paths into the immutable runtime store.

use super::Installations;
use super::catalog::{self, State};
use super::filesystem::{self, invalid};
use crate::bundles::{MANIFEST_FILE, MAX_MANIFEST_BYTES, Manifest, valid_digest};
use crate::private_fs::{options, sync_directory};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

const SNAPSHOT: &str = ".layout.json";
const MAX_REFERENCES: usize = 8192;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const MAX_VERSION_PATH_BYTES: usize = 240;

type References = BTreeMap<String, String>;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
  previous: References,
  pending: Option<References>,
}

pub(super) fn refresh(store: &Installations, state: &State) {
  if let Err(error) = sync(store, state) {
    eprintln!("ctlup: installation paths could not be refreshed: {error}");
  }
}

/// Projects authoritative catalog entries while the caller holds the store lock.
/// The snapshot tracks ownership and makes an interrupted refresh retryable.
pub(super) fn sync(store: &Installations, state: &State) -> io::Result<()> {
  let root = store.root.join("installs");
  filesystem::directory(&root)?;
  let desired = references(&root, state)?;
  let snapshot_path = root.join(SNAPSHOT);
  let snapshot = read_json::<Snapshot>(&snapshot_path, MAX_SNAPSHOT_BYTES)?.unwrap_or_default();
  validate_references(&snapshot.previous)?;
  if let Some(pending) = &snapshot.pending {
    validate_references(pending)?;
  }

  let actual = inspect_references(&root, &snapshot, &desired)?;
  if snapshot.pending.is_none() && actual == desired && snapshot.previous == desired {
    return Ok(());
  }
  write_json(
    &snapshot_path,
    &Snapshot {
      previous: actual.clone(),
      pending: Some(desired.clone()),
    },
    MAX_SNAPSHOT_BYTES,
  )?;

  // Remove stale references first so an interrupted refresh never accumulates
  // several generations of paths. Immutable payloads have separate retention.
  for (name, id) in &actual {
    if !desired.contains_key(name) && read_reference(&root, name)?.as_ref() == Some(id) {
      let path = reference_path(&root, name);
      fs::remove_file(&path)?;
      remove_empty_parents(&root, path.parent().unwrap())?;
    }
  }
  for (name, id) in &desired {
    let actual_id = actual.get(name);
    if actual_id != Some(id) {
      if read_reference(&root, name)?.as_ref() != actual_id {
        return Err(invalid("installation reference changed during refresh"));
      }
      update_reference(&root, name, id)?;
    }
  }
  write_json(
    &snapshot_path,
    &Snapshot {
      previous: desired,
      pending: None,
    },
    MAX_SNAPSHOT_BYTES,
  )
}

fn inspect_references(
  root: &Path,
  snapshot: &Snapshot,
  desired: &References,
) -> io::Result<References> {
  let pending = snapshot.pending.as_ref();
  let names: BTreeSet<_> = snapshot
    .previous
    .keys()
    .chain(pending.into_iter().flat_map(References::keys))
    .chain(desired.keys())
    .collect();
  let mut actual = References::new();
  for name in names {
    if let Some(id) = read_reference(root, name)? {
      if snapshot.previous.get(name) != Some(&id)
        && pending.and_then(|pending| pending.get(name)) != Some(&id)
      {
        return Err(invalid(
          "installation reference conflicts with an existing path",
        ));
      }
      actual.insert(name.clone(), id);
    }
  }
  Ok(actual)
}

fn references(root: &Path, state: &State) -> io::Result<References> {
  let development_ids: BTreeSet<_> = state.development.values().collect();
  let mut chosen = BTreeMap::new();
  let mut manifests = BTreeMap::<&str, Manifest>::new();
  for entry in &state.entries {
    let Some(origin) = &entry.origin else {
      return Err(invalid("installation reference lacks catalog provenance"));
    };
    let channel = origin.channel.as_str();
    if channel == "local" && (!entry.current || development_ids.contains(&entry.bundle_id)) {
      continue;
    }
    let manifest = match manifests.entry(&entry.bundle_id) {
      std::collections::btree_map::Entry::Occupied(existing) => existing.into_mut(),
      std::collections::btree_map::Entry::Vacant(slot) => {
        if !valid_digest(&entry.bundle_id) {
          return Err(invalid("invalid installation reference identity"));
        }
        let payload = root.join(&entry.bundle_id);
        filesystem::directory(&payload)?;
        let manifest: Manifest = read_json(&payload.join(MANIFEST_FILE), MAX_MANIFEST_BYTES)?
          .ok_or_else(|| invalid("installation reference payload has no manifest"))?;
        manifest.validate()?;
        if manifest.bundle_id != entry.bundle_id {
          return Err(invalid("installation reference manifest identity changed"));
        }
        slot.insert(manifest)
      }
    };
    if manifest.target_triple != entry.target_triple {
      return Err(invalid("installation reference target changed"));
    }
    let version = version_label(manifest, channel, origin.publication_id.as_deref());
    let target = &entry.target_triple;
    let name = match channel {
      "stable" | "nightly" => format!("{channel}/{version}/{target}"),
      "local" => format!("dev/unassigned/{version}/{target}"),
      other => format!("dev/{other}/{version}/{target}"),
    };
    // A publication can be rebuilt at the same version. Show the channel's
    // selected build, then the most recent retained build when there is a tie.
    let date = origin.built_at.as_deref().and_then(|date| {
      time::OffsetDateTime::parse(date, &time::format_description::well_known::Rfc3339).ok()
    });
    let candidate = (entry.current, date, entry.bundle_id.as_str());
    let selected = chosen.entry(name).or_insert(candidate);
    *selected = (*selected).max(candidate);
  }
  let mut references: References = chosen
    .into_iter()
    .map(|(name, (_, _, id))| (name, id.to_owned()))
    .collect();
  for (checkout, id) in &state.development {
    references.insert(format!("dev/{checkout}"), id.clone());
  }
  validate_references(&references)?;
  Ok(references)
}

fn version_label(manifest: &Manifest, channel: &str, publication: Option<&str>) -> String {
  let build = manifest.components["ctl-agent"].build.version.as_str();
  let publication_version = if channel == "nightly" {
    [manifest.distribution_id.as_deref(), publication]
      .into_iter()
      .flatten()
      .find(|candidate| semver::Version::parse(candidate).is_ok())
  } else {
    None
  };
  let version = publication_version.unwrap_or(build);
  if semver::Version::parse(version).is_ok() {
    let mut label = String::with_capacity(version.len());
    for byte in version.bytes() {
      if byte.is_ascii_uppercase() {
        // SemVer identifiers are case-sensitive. Escape uppercase bytes so two
        // otherwise identical versions work on case-insensitive volumes too.
        use std::fmt::Write as _;
        write!(label, "_{byte:02x}").unwrap();
      } else {
        label.push(char::from(byte));
      }
    }
    if label.len() <= MAX_VERSION_PATH_BYTES {
      return label;
    }
  }
  format!("build-{}", manifest.bundle_id)
}

fn validate_references(references: &References) -> io::Result<()> {
  if references.len() > MAX_REFERENCES {
    return Err(invalid("installation layout contains too many references"));
  }
  for (name, id) in references {
    let segments: Vec<_> = name.splitn(5, '/').collect();
    let valid = match segments.as_slice() {
      ["dev", checkout] => valid_digest(checkout),
      ["stable" | "nightly", version, target] => valid_version_target(version, target),
      ["dev", channel, version, target] => {
        (matches!(*channel, "provided" | "unassigned")
          || (channel.starts_with("pr-") && catalog::validate_channel(channel).is_ok()))
          && valid_version_target(version, target)
      }
      _ => false,
    };
    if !valid || !valid_digest(id) {
      return Err(invalid("invalid installation layout reference"));
    }
  }
  Ok(())
}

fn valid_version_target(version: &str, target: &str) -> bool {
  crate::bundles::validate_target(target).is_ok()
    && (version.strip_prefix("build-").is_some_and(valid_digest)
      || (version.len() <= MAX_VERSION_PATH_BYTES
        && version.starts_with(|c: char| c.is_ascii_digit())
        && !version.ends_with('.')
        && version.bytes().all(|byte| {
          byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._+-".contains(&byte)
        })))
}

fn reference_path(root: &Path, name: &str) -> PathBuf {
  let path = root.join(name);
  #[cfg(windows)]
  let path = path.with_extension("json");
  path
}

fn relative_payload(name: &str, id: &str) -> PathBuf {
  let mut relative = PathBuf::new();
  for _ in name.bytes().filter(|byte| *byte == b'/') {
    relative.push("..");
  }
  relative.join(id)
}

fn safe_parents(root: &Path, path: &Path, create: bool) -> io::Result<bool> {
  let parents = path
    .parent()
    .unwrap()
    .strip_prefix(root)
    .map_err(io::Error::other)?;
  let mut directory = root.to_path_buf();
  for part in parents {
    directory.push(part);
    if create {
      filesystem::ensure_directory(&directory)?;
    } else if filesystem::path_exists(&directory)? {
      filesystem::directory(&directory)?;
    } else {
      return Ok(false);
    }
  }
  Ok(true)
}

fn remove_empty_parents(root: &Path, mut parent: &Path) -> io::Result<()> {
  while parent != root {
    filesystem::directory(parent)?;
    match fs::remove_dir(parent) {
      Ok(()) => {
        parent = parent.parent().unwrap();
        sync_directory(parent)?;
      }
      Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => break,
      Err(error) => return Err(error),
    }
  }
  Ok(())
}

#[cfg(windows)]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReferenceFile {
  bundle_id: String,
  relative_path: PathBuf,
}

fn read_reference(root: &Path, name: &str) -> io::Result<Option<String>> {
  let path = reference_path(root, name);
  if !safe_parents(root, &path, false)? || !filesystem::path_exists(&path)? {
    return Ok(None);
  }
  #[cfg(unix)]
  let (id, relative) = {
    if !fs::symlink_metadata(&path)?.is_symlink() {
      return Err(invalid("installation reference is not a managed link"));
    }
    let relative = fs::read_link(&path)?;
    let id = relative
      .file_name()
      .and_then(|name| name.to_str())
      .ok_or_else(|| invalid("invalid installation reference link"))?
      .to_owned();
    (id, relative)
  };
  #[cfg(windows)]
  let (id, relative) = {
    let reference: ReferenceFile =
      read_json(&path, 1024)?.ok_or_else(|| invalid("installation reference disappeared"))?;
    (reference.bundle_id, reference.relative_path)
  };
  if !valid_digest(&id) || relative.as_os_str() != relative_payload(name, &id).as_os_str() {
    return Err(invalid(
      "installation reference points outside its immutable payload",
    ));
  }
  Ok(Some(id))
}

fn update_reference(root: &Path, name: &str, id: &str) -> io::Result<()> {
  let path = reference_path(root, name);
  safe_parents(root, &path, true)?;
  #[cfg(unix)]
  {
    let next =
      filesystem::Temporary(path.with_file_name(format!(".layout-next-{}", uuid::Uuid::new_v4())));
    std::os::unix::fs::symlink(relative_payload(name, id), &next.0)?;
    fs::rename(&next.0, &path)?;
    sync_directory(path.parent().unwrap())
  }
  #[cfg(windows)]
  write_json(
    &path,
    &ReferenceFile {
      bundle_id: id.to_owned(),
      relative_path: relative_payload(name, id),
    },
    1024,
  )
}

fn read_json<T: DeserializeOwned>(path: &Path, maximum: usize) -> io::Result<Option<T>> {
  let metadata = match fs::symlink_metadata(path) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  filesystem::private_file(&metadata)?;
  let file = options().read(true).open(path)?;
  filesystem::private_file(&file.metadata()?)?;
  let mut bytes = Vec::new();
  file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
  if bytes.len() > maximum {
    return Err(invalid(
      "installation layout metadata exceeds its size limit",
    ));
  }
  serde_json::from_slice(&bytes)
    .map(Some)
    .map_err(io::Error::other)
}

fn write_json(path: &Path, value: &impl Serialize, maximum: usize) -> io::Result<()> {
  let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
  if bytes.len() > maximum {
    return Err(invalid(
      "installation layout metadata exceeds its size limit",
    ));
  }
  if filesystem::path_exists(path)? {
    filesystem::private_file(&fs::symlink_metadata(path)?)?;
  }
  let next =
    filesystem::Temporary(path.with_file_name(format!(".layout-next-{}", uuid::Uuid::new_v4())));
  let mut file = options().write(true).create_new(true).open(&next.0)?;
  file.write_all(&bytes)?;
  file.sync_all()?;
  drop(file);
  fs::rename(&next.0, path)?;
  sync_directory(path.parent().unwrap())
}

#[cfg(test)]
mod tests;
