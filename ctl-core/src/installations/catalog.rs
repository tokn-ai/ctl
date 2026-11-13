//! Local provenance is separate from immutable, externally published manifests.

use super::Installations;
use super::filesystem::{self, invalid};
use crate::bundles::Bundle;
use crate::private_fs::{options, sync_directory};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::Path;

const CATALOG: &str = "catalog.json";
const JOURNAL: &str = ".catalog-transaction.json";
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_ENTRIES: usize = 4096;

/// The publication or explicit source that supplied installed target bundles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogOrigin {
  pub channel: String,
  pub publication_id: Option<String>,
  pub built_at: Option<String>,
}

impl CatalogOrigin {
  /// Validates bounded provenance, including a UTC RFC3339 build timestamp.
  ///
  /// # Errors
  /// Rejects unknown channels, unsafe publication identities or invalid dates.
  pub fn validate(&self) -> io::Result<()> {
    validate_channel(&self.channel)?;
    if self.publication_id.as_ref().is_some_and(|id| {
      id.is_empty()
        || id.len() > 256
        || !id
          .bytes()
          .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
    }) || self
      .built_at
      .as_deref()
      .is_some_and(|date| !utc_timestamp(date))
    {
      return Err(invalid("invalid installation provenance"));
    }
    Ok(())
  }
}

/// An installed target and its known provenance. Unknown historical payloads
/// have no origin and are never inferred to belong to the saved default channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
  pub bundle_id: String,
  pub target_triple: String,
  pub origin: Option<CatalogOrigin>,
  pub current: bool,
}

impl CatalogEntry {
  /// Validates the bounded entry without trusting or opening a payload path.
  ///
  /// # Errors
  /// Rejects malformed identities, targets or provenance.
  pub fn validate(&self) -> io::Result<()> {
    if !filesystem::valid_id(&self.bundle_id) || (self.current && self.origin.is_none()) {
      return Err(invalid("invalid installation catalog entry"));
    }
    crate::bundles::validate_target(&self.target_triple)?;
    if let Some(origin) = &self.origin {
      origin.validate()?;
    }
    Ok(())
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
  schema_version: u16,
  pub(super) entries: Vec<CatalogEntry>,
}

impl Default for State {
  fn default() -> Self {
    Self {
      schema_version: 1,
      entries: Vec::new(),
    }
  }
}

impl State {
  fn validate(&self, store: &Installations) -> io::Result<()> {
    self.validate_index()?;
    for (id, target) in self
      .entries
      .iter()
      .map(|entry| (&entry.bundle_id, &entry.target_triple))
      .collect::<BTreeMap<_, _>>()
    {
      let bundle = filesystem::load(store, id)?;
      if bundle.manifest.target_triple != *target {
        return Err(invalid("catalog target differs from its immutable payload"));
      }
    }
    Ok(())
  }

  fn validate_index(&self) -> io::Result<()> {
    if self.schema_version != 1 || self.entries.len() > MAX_ENTRIES {
      return Err(invalid("unsupported or oversized installation catalog"));
    }
    let mut unique = BTreeSet::new();
    let mut heads = BTreeSet::new();
    let mut publications = BTreeMap::new();
    let mut targets = BTreeMap::new();
    for entry in &self.entries {
      entry.validate()?;
      let Some(origin) = &entry.origin else {
        return Err(invalid(
          "stored installation catalog entries require provenance",
        ));
      };
      if !unique.insert((&origin.channel, &entry.target_triple, &entry.bundle_id))
        || (entry.current && !heads.insert((&origin.channel, &entry.target_triple)))
        || targets
          .insert(&entry.bundle_id, &entry.target_triple)
          .is_some_and(|target| target != &entry.target_triple)
      {
        return Err(invalid("duplicate installation catalog entries"));
      }
      if entry.current
        && publications
          .insert(&origin.channel, &origin.publication_id)
          .is_some_and(|previous| previous != &origin.publication_id)
      {
        return Err(invalid(
          "a channel contains targets from different publications",
        ));
      }
    }
    Ok(())
  }

  pub(super) fn register(&mut self, bundles: &[Bundle], origin: &CatalogOrigin, merge: bool) {
    let same_publication = merge
      && origin.publication_id.as_ref().is_some_and(|id| {
        self
          .entries
          .iter()
          .filter(|entry| entry.current)
          .any(|entry| {
            entry.origin.as_ref().is_some_and(|existing| {
              existing.channel == origin.channel && existing.publication_id.as_ref() == Some(id)
            })
          })
      });
    for entry in &mut self.entries {
      if entry
        .origin
        .as_ref()
        .is_some_and(|existing| existing.channel == origin.channel)
        && (!same_publication
          || bundles
            .iter()
            .any(|bundle| bundle.manifest.target_triple == entry.target_triple))
      {
        entry.current = false;
      }
    }
    for bundle in bundles {
      self.entries.retain(|entry| {
        entry.bundle_id != bundle.manifest.bundle_id
          || entry
            .origin
            .as_ref()
            .is_none_or(|existing| existing.channel != origin.channel)
      });
      self.entries.push(CatalogEntry {
        bundle_id: bundle.manifest.bundle_id.clone(),
        target_triple: bundle.manifest.target_triple.clone(),
        origin: Some(origin.clone()),
        current: true,
      });
    }
    self.entries.sort_by(|left, right| {
      (
        &left.origin.as_ref().map(|origin| &origin.channel),
        &left.target_triple,
        &left.bundle_id,
      )
        .cmp(&(
          &right.origin.as_ref().map(|origin| &origin.channel),
          &right.target_triple,
          &right.bundle_id,
        ))
    });
  }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transaction {
  catalog: State,
  active: Option<String>,
  previous: Option<String>,
}

impl Transaction {
  fn validate(&self, store: &Installations) -> io::Result<()> {
    self.validate_index()?;
    self.catalog.validate(store)?;
    for id in [self.active.as_deref(), self.previous.as_deref()]
      .into_iter()
      .flatten()
    {
      filesystem::validate_native(&filesystem::load(store, id)?)?;
    }
    Ok(())
  }

  fn validate_index(&self) -> io::Result<()> {
    self.catalog.validate_index()?;
    let Some(id) = &self.active else {
      return if self.previous.is_none() {
        Ok(())
      } else {
        Err(invalid(
          "catalog transaction has rollback without activation",
        ))
      };
    };
    if self.previous.as_ref() == Some(id) {
      return Err(invalid(
        "catalog activation and rollback identities must differ",
      ));
    }
    for id in [self.active.as_deref(), self.previous.as_deref()]
      .into_iter()
      .flatten()
    {
      if !self
        .catalog
        .entries
        .iter()
        .any(|entry| entry.bundle_id == id && crate::bundles::local_target(&entry.target_triple))
      {
        return Err(invalid(
          "catalog transaction selection must name a recorded native build",
        ));
      }
    }
    Ok(())
  }
}

pub(super) fn validate_channel(channel: &str) -> io::Result<()> {
  let pr = channel.strip_prefix("pr-").is_some_and(|number| {
    !number.is_empty()
      && !number.starts_with('0')
      && number.len() <= 20
      && number.bytes().all(|byte| byte.is_ascii_digit())
      && number.parse::<u64>().is_ok()
  });
  if !matches!(channel, "stable" | "nightly" | "local" | "provided") && !pr {
    return Err(invalid("invalid installation catalog channel"));
  }
  Ok(())
}

fn utc_timestamp(date: &str) -> bool {
  (20..=30).contains(&date.len())
    && date.ends_with('Z')
    && time::OffsetDateTime::parse(date, &time::format_description::well_known::Rfc3339)
      .is_ok_and(|date| date.offset() == time::UtcOffset::UTC)
}

pub(super) fn exists(store: &Installations) -> io::Result<bool> {
  filesystem::path_exists(&store.root.join(CATALOG))
}

pub(super) fn read(store: &Installations) -> io::Result<State> {
  let state = read_index(store)?;
  state.validate(store)?;
  Ok(state)
}

pub(super) fn read_index(store: &Installations) -> io::Result<State> {
  let state = existing_state(store)?;
  if state.is_none() && has_installation_data(store)? {
    return Err(invalid(
      "existing installations have no authoritative catalog; run ctlup migrate",
    ));
  }
  let state = state.unwrap_or_default();
  state.validate_index()?;
  Ok(state)
}

pub(super) fn read_for_migration(store: &Installations) -> io::Result<State> {
  let state = existing_state(store)?.unwrap_or_default();
  state.validate(store)?;
  Ok(state)
}

fn existing_state(store: &Installations) -> io::Result<Option<State>> {
  // A durable journal is the committed intent after interruption. Inspection
  // uses its complete catalog without mutating pointers or hiding corruption.
  if let Some(transaction) = read_file::<Transaction>(&store.root.join(JOURNAL))? {
    transaction.validate_index()?;
    Ok(Some(transaction.catalog))
  } else {
    read_file(&store.root.join(CATALOG))
  }
}

fn has_installation_data(store: &Installations) -> io::Result<bool> {
  for name in ["current", "previous", ".activation.json"] {
    if filesystem::path_exists(&store.root.join(name))? {
      return Ok(true);
    }
  }
  let target = match crate::paths::native_target() {
    "x86_64-unknown-linux-gnu" => "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-gnu" => "aarch64-unknown-linux-musl",
    target => target,
  };
  if filesystem::path_exists(
    &store
      .root
      .join("components/selected")
      .join(format!("local-{target}.json")),
  )? {
    return Ok(true);
  }
  Ok(!filesystem::entries(store)?.is_empty())
}

pub(super) fn inventory(store: &Installations) -> io::Result<Vec<CatalogEntry>> {
  read(store).map(|state| state.entries)
}

pub(super) fn initialize(store: &Installations, state: &State) -> io::Result<()> {
  if !exists(store)? {
    if !state.entries.is_empty() {
      return Err(invalid(
        "catalog initialization requires a verified empty state",
      ));
    }
    // The caller holds the installation lock and has already rejected existing
    // uncatalogued installations. Interrupted first writes may now leave only
    // ordinary unregistered payloads, rather than an ambiguous old store.
    commit(store, state, None)?;
  }
  Ok(())
}

pub(super) fn commit(
  store: &Installations,
  state: &State,
  activation: Option<(&Bundle, Option<&Bundle>)>,
) -> io::Result<()> {
  state.validate(store)?;
  if let Some((active, previous)) = activation {
    filesystem::validate_native(active)?;
    if let Some(previous) = previous {
      filesystem::validate_native(previous)?;
    }
    // Detect command collisions before recording durable activation intent.
    // Otherwise a fixable collision could leave every later operation trying
    // to replay an activation that could never have started successfully.
    filesystem::prepare_activation(store)?;
  }
  let transaction = Transaction {
    catalog: state.clone(),
    active: activation.map(|(bundle, _)| bundle.manifest.bundle_id.clone()),
    previous: activation.and_then(|(active, previous)| {
      previous
        .filter(|previous| previous.manifest.bundle_id != active.manifest.bundle_id)
        .map(|bundle| bundle.manifest.bundle_id.clone())
    }),
  };
  if filesystem::path_exists(&store.root.join(JOURNAL))? {
    return Err(invalid(
      "installation catalog transaction was not recovered",
    ));
  }
  replace(&store.root.join(JOURNAL), &transaction)?;
  recover(store)
}

pub(super) fn recover(store: &Installations) -> io::Result<()> {
  let transaction = read_file::<Transaction>(&store.root.join(JOURNAL))?;
  if let Some(transaction) = &transaction {
    transaction.validate(store)?;
  }
  #[cfg(unix)]
  {
    if transaction.is_none() && filesystem::path_exists(&store.root.join(".activation.json"))? {
      // A standalone native activation still belongs to this authoritative
      // catalog. Reject damaged recorded payloads before changing selection.
      read(store)?;
    }
    filesystem::recover(store)?;
  }
  let Some(transaction) = transaction else {
    return Ok(());
  };
  if let Some(id) = &transaction.active {
    let active = filesystem::load(store, id)?;
    filesystem::validate_native(&active)?;
    let previous = transaction
      .previous
      .as_deref()
      .map(|id| filesystem::load(store, id))
      .transpose()?;
    if let Some(previous) = &previous {
      filesystem::validate_native(previous)?;
    }
    filesystem::activate(store, &active, previous.as_ref())?;
  }
  replace(&store.root.join(CATALOG), &transaction.catalog)?;
  fs::remove_file(store.root.join(JOURNAL))?;
  sync_directory(&store.root)
}

fn read_file<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<Option<T>> {
  match fs::symlink_metadata(path) {
    Ok(metadata) => filesystem::private_file(&metadata)?,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  }
  let file = options().read(true).open(path)?;
  filesystem::private_file(&file.metadata()?)?;
  let mut bytes = Vec::new();
  file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
  if bytes.len() as u64 > MAX_BYTES {
    return Err(invalid("installation catalog exceeds its size limit"));
  }
  serde_json::from_slice(&bytes)
    .map(Some)
    .map_err(io::Error::other)
}

pub(super) fn replace(path: &Path, value: &impl Serialize) -> io::Result<()> {
  let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
  if bytes.len() as u64 > MAX_BYTES {
    return Err(invalid("installation catalog exceeds its size limit"));
  }
  if filesystem::path_exists(path)? {
    filesystem::private_file(&fs::symlink_metadata(path)?)?;
  }
  let temporary =
    filesystem::Temporary(path.with_file_name(format!(".catalog-next-{}", uuid::Uuid::new_v4())));
  let mut file = options().write(true).create_new(true).open(&temporary.0)?;
  file.write_all(&bytes)?;
  file.sync_all()?;
  drop(file);
  fs::rename(&temporary.0, path)?;
  sync_directory(path.parent().unwrap())
}

#[cfg(test)]
mod tests;
