use sha2::{Digest, Sha256};

use super::{Entry, Source, selected};

const MIN_DIGEST_LENGTH: usize = 12;
const DIGEST_LENGTH: usize = 64;
const PREFIX_LENGTH: usize = 2;

pub(super) fn key(id: &str, source: Source) -> String {
  let prefix = match source {
    Source::Credential => "p",
    Source::Identity => "k",
  };
  format!("{prefix}-{:x}", Sha256::digest(id.as_bytes()))
}

/// Use the shortest unambiguous digest prefix, including conflicts with names.
/// A full digest collision cannot safely identify an item, so retain its ID.
pub(super) fn short_id(entry: &Entry, entries: &[Entry]) -> String {
  if !valid_alias(&entry.reference) || entry.reference.len() != PREFIX_LENGTH + DIGEST_LENGTH {
    return entry.id.clone();
  }
  for length in MIN_DIGEST_LENGTH..=DIGEST_LENGTH {
    let candidate = &entry.reference[..PREFIX_LENGTH + length];
    if entries.iter().all(|other| {
      other.id == entry.id || (!other.reference.starts_with(candidate) && other.name != candidate)
    }) {
      return candidate.into();
    }
  }
  entry.id.clone()
}

/// Reserve alias syntax even when the referenced item is gone. Falling back to
/// names could make a previously displayed ID select an unrelated item later.
pub(super) fn select<'a>(
  entries: &'a [Entry],
  selector: &str,
) -> Option<Result<&'a Entry, String>> {
  if !valid_alias(selector) {
    return None;
  }
  let matches: Vec<_> = entries
    .iter()
    .filter(|entry| entry.reference.starts_with(selector))
    .collect();
  if let [entry] = matches.as_slice()
    && let Some(other) = entries
      .iter()
      .find(|other| other.id != entry.id && other.name == selector)
  {
    return Some(selected(&[entry, other], selector));
  }
  Some(selected(&matches, selector))
}

fn valid_alias(selector: &str) -> bool {
  let Some(digest) = selector
    .strip_prefix("p-")
    .or_else(|| selector.strip_prefix("k-"))
  else {
    return false;
  };
  (MIN_DIGEST_LENGTH..=DIGEST_LENGTH).contains(&digest.len())
    && digest
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests;
