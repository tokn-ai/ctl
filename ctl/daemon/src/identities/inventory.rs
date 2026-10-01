use super::{IdentityError, SavedIdentity, files, inspect_path};
use ctl_ipc::identities::{FileState, IdentityFile, Inventory, MAX_PATHS, PassphraseState};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub(super) fn list(paths: &[String]) -> Result<Inventory, IdentityError> {
  if paths.len() > MAX_PATHS {
    return Err(IdentityError::InvalidRequest);
  }
  #[cfg(target_os = "macos")]
  let stored = crate::keychain::identity::list().and_then(|(saved, complete)| {
    crate::keychain::metadata_import_required()
      .map(|required| (saved, complete, required))
      .map_err(|error| crate::keychain::identity::map_error(error, IdentityError::ListFailed))
  });
  #[cfg(not(target_os = "macos"))]
  let stored: Result<(HashMap<String, SavedIdentity>, bool, bool), IdentityError> =
    Err(IdentityError::KeychainUnavailable);
  let (saved, complete, keychain_error, available, metadata_import_required) = match stored {
    Ok((saved, complete, required)) => (saved, complete, None, true, required),
    Err(error) => (HashMap::new(), false, Some(error), false, false),
  };
  let saved_metadata_complete = available && complete && !metadata_import_required;
  let mut file_discovery_complete = true;
  let mut candidates = paths.to_vec();
  candidates.extend(saved.values().map(|metadata| metadata.path.clone()));
  if let Some(home) = dirs::home_dir() {
    match std::fs::read_dir(home.join(".ssh")) {
      Ok(entries) => {
        for (index, entry) in entries.take(MAX_PATHS + 1).enumerate() {
          if index == MAX_PATHS {
            file_discovery_complete = false;
            break;
          }
          let Ok(entry) = entry else {
            file_discovery_complete = false;
            continue;
          };
          let path = entry.path();
          if path.extension().is_some_and(|ext| ext == "pub") {
            continue;
          }
          if let Some(path) = path.to_str()
            && inspect_path(path).is_ok()
          {
            candidates.push(path.to_owned());
          }
        }
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(_) => file_discovery_complete = false,
    }
  }
  let identity_files = project(
    &candidates,
    &saved,
    saved_metadata_complete,
    &mut file_discovery_complete,
  );
  Ok(Inventory {
    identity_files,
    complete: complete && file_discovery_complete && !metadata_import_required,
    file_discovery_complete,
    warning: keychain_error.map(|error| error.to_string()).or_else(|| {
      (!file_discovery_complete).then(|| {
        "Some identity files could not be listed. The identity inventory is limited to 512 files."
          .into()
      })
    }),
    keychain_available: available,
    keychain_error: keychain_error.map(|error| error.code().into()),
    metadata_import_required,
  })
}

pub(super) fn project(
  paths: &[String],
  saved: &HashMap<String, SavedIdentity>,
  available: bool,
  complete: &mut bool,
) -> Vec<IdentityFile> {
  let mut records = BTreeMap::new();
  for path in paths {
    let record = match inspect_path(path) {
      Ok(snapshot) => {
        let mut record = snapshot.record(available);
        if let Some(metadata) = saved.get(&snapshot.identity_id) {
          if metadata.file_version == snapshot.file_version {
            record.passphrase_state = if snapshot.encrypted {
              PassphraseState::Saved
            } else {
              PassphraseState::NotRequired
            };
            record.key_type = Some(metadata.key_type.clone());
            record.fingerprint = Some(metadata.fingerprint.clone());
          } else {
            record.passphrase_state = PassphraseState::FileChanged;
            record.detail = Some("The file changed since its passphrase was saved. Verify and save its current passphrase before reuse.".into());
          }
        }
        record
      }
      Err(error) => {
        let Ok(expanded) = files::expand_path(path) else {
          *complete = false;
          continue;
        };
        // A missing leaf can still have a canonical parent (notably /tmp and
        // /var aliases on macOS). Keep its saved identity stable after removal.
        let expanded = match (expanded.parent(), expanded.file_name()) {
          (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map_or_else(|_| expanded.clone(), |parent| parent.join(name)),
          _ => expanded,
        };
        let path = expanded.to_string_lossy().into_owned();
        let identity_id = files::digest(path.as_bytes());
        let metadata = saved.get(&identity_id);
        IdentityFile {
          identity_id,
          display_path: files::display_path(&path),
          path,
          file_version: None,
          // Saved metadata belongs to the previous snapshot, not to a missing
          // or unreadable current file. Do not label it as currently verified.
          key_type: None,
          fingerprint: None,
          encrypted: metadata.map(|_| true),
          file_state: match error {
            IdentityError::MissingFile => FileState::Missing,
            IdentityError::UnsupportedFile => FileState::Unsupported,
            _ => FileState::Unreadable,
          },
          passphrase_state: if metadata.is_some() {
            PassphraseState::FileChanged
          } else {
            PassphraseState::Unknown
          },
          detail: Some(error.to_string()),
        }
      }
    };
    if records.len() >= MAX_PATHS && !records.contains_key(&record.identity_id) {
      *complete = false;
      continue;
    }
    records.insert(record.identity_id.clone(), record);
  }
  let mut result: Vec<_> = records.into_values().collect();
  result.sort_by(|left, right| {
    let basename = |path: &str| {
      Path::new(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
    };
    basename(&left.path)
      .cmp(&basename(&right.path))
      .then_with(|| left.path.cmp(&right.path))
  });
  result
}

#[cfg(test)]
mod tests;
