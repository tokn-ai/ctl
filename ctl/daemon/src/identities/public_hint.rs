//! Public hints select candidate keys; local unlock and exact public-key
//! comparison must still authorize every use of the corresponding secret.

use std::io::Read as _;

use base64::Engine as _;

use super::{IdentitySnapshot, SavedIdentity, ensure_current, files};

// Bound public-file parsing and optional index metadata independently of the
// larger private-key snapshot limit. Oversized hints use native SSH auth.
pub(super) const MAX_PUBLIC_KEY_BYTES: usize = 8 * 1024;

pub(super) struct PublicKey {
  pub canonical: String,
  pub key_type: String,
  pub fingerprint: String,
}

pub(super) fn parse(text: &str) -> Option<PublicKey> {
  if text.len() > MAX_PUBLIC_KEY_BYTES || text.trim().lines().count() != 1 {
    return None;
  }
  let mut fields = text.split_ascii_whitespace();
  let declared_type = fields.next()?;
  let encoded = fields.next()?;
  let blob = base64::engine::general_purpose::STANDARD
    .decode(encoded)
    .ok()?;
  let (key_type, fingerprint) = files::public_metadata(&blob).ok()?;
  if declared_type != key_type || blob.len() <= 4 + key_type.len() {
    return None;
  }
  Some(PublicKey {
    canonical: format!(
      "{key_type} {}",
      base64::engine::general_purpose::STANDARD.encode(blob)
    ),
    key_type,
    fingerprint,
  })
}

/// Return public metadata without retrieving a passphrase or unlocking a key.
/// The hint is advisory and must be compared with the locally unlocked key.
#[must_use]
pub fn public_key_hint(snapshot: &IdentitySnapshot) -> Option<String> {
  hint(snapshot, None, || saved_metadata(snapshot))
}

/// Offer a key for lazy unlock only when a matching saved passphrase is known
/// noninteractively. Missing or unavailable metadata preserves native SSH auth.
#[must_use]
pub fn saved_public_key_hint(snapshot: &IdentitySnapshot) -> Option<String> {
  let metadata = saved_metadata(snapshot)?;
  saved_hint(snapshot, &metadata)
}

fn saved_hint(snapshot: &IdentitySnapshot, metadata: &SavedIdentity) -> Option<String> {
  if !snapshot.encrypted || !metadata.matches(snapshot) {
    return None;
  }
  hint(snapshot, Some(metadata), || Some(metadata.clone()))
}

fn saved_metadata(snapshot: &IdentitySnapshot) -> Option<SavedIdentity> {
  #[cfg(target_os = "macos")]
  return crate::keychain::identity::saved_metadata(snapshot)
    .ok()
    .flatten();
  #[cfg(not(target_os = "macos"))]
  {
    let _ = snapshot;
    None
  }
}

fn hint(
  snapshot: &IdentitySnapshot,
  expected: Option<&SavedIdentity>,
  metadata: impl FnOnce() -> Option<SavedIdentity>,
) -> Option<String> {
  ensure_current(snapshot).ok()?;
  let matching = |key: &str| {
    let public = matching(snapshot, key)?;
    if let Some(expected) = expected {
      let parsed = parse(&public)?;
      if parsed.key_type != expected.key_type || parsed.fingerprint != expected.fingerprint {
        return None;
      }
    }
    Some(public)
  };
  if let Some(public) = snapshot.public_key.as_deref().and_then(matching) {
    return Some(public);
  }
  // An OpenSSH envelope already identifies its public key. A sibling .pub
  // must never override a conflicting or oversized envelope.
  if snapshot.public_key.is_some() {
    return None;
  }
  if let Some(public) = sibling(snapshot).and_then(|key| matching(&key)) {
    return Some(public);
  }
  let metadata = metadata()?;
  if !metadata.matches(snapshot) {
    return None;
  }
  matching(metadata.public_key.as_deref()?)
}

fn matching(snapshot: &IdentitySnapshot, text: &str) -> Option<String> {
  let public = parse(text)?;
  if snapshot
    .key_type
    .as_ref()
    .is_some_and(|key_type| *key_type != public.key_type)
    || snapshot
      .fingerprint
      .as_ref()
      .is_some_and(|fingerprint| *fingerprint != public.fingerprint)
  {
    return None;
  }
  Some(public.canonical)
}

fn sibling(snapshot: &IdentitySnapshot) -> Option<String> {
  let mut options = std::fs::OpenOptions::new();
  options.read(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.custom_flags(i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).ok()?);
  }
  let file = options.open(format!("{}.pub", snapshot.path)).ok()?;
  let metadata = file.metadata().ok()?;
  if !metadata.is_file() || metadata.len() > MAX_PUBLIC_KEY_BYTES as u64 {
    return None;
  }
  let mut public = String::new();
  file
    .take((MAX_PUBLIC_KEY_BYTES + 1) as u64)
    .read_to_string(&mut public)
    .ok()?;
  (public.len() <= MAX_PUBLIC_KEY_BYTES).then_some(public)
}

#[cfg(test)]
mod tests;
