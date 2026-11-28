//! Local provenance is separate from immutable, externally published manifests.

use super::Installations;
use super::filesystem::{self, invalid};
use crate::bundles::Bundle;
use crate::component::ComponentBuildInfo;
use crate::component_update::BundleChannel;
use crate::private_fs::{options, sync_directory};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::Path;

const CATALOG: &str = "installations.json";
const RUNTIME_PATHS: &str = "runtime-paths.json";
const UPDATER_PATHS: &str = "updater-paths.json";
const RUNTIME_BUILD_PATHS: &str = "runtime-build-paths.json";
const UPDATER_BUILD_PATHS: &str = "updater-build-paths.json";
const UPDATER_OBJECT_PATHS: &str = "updater-object-paths.json";
const UPDATER_SELECTION: &str = "updater-selection.json";
const DESKTOP_OBJECT_PATHS: &str = "desktop-object-paths.json";
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;

/// The publication or explicit source that supplied an installed build.
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

/// Identity and source of the updater selected for future invocations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdaterSelection {
  pub content_id: String,
  pub build: ComponentBuildInfo,
  pub origin: Option<CatalogOrigin>,
  /// Explicitly followed channel, independent of the runtime's source.
  pub tracked_channel: Option<BundleChannel>,
}

impl UpdaterSelection {
  pub(super) fn validate(&self) -> io::Result<()> {
    if !crate::bundles::valid_digest(&self.content_id) || !self.build.is_valid() {
      return Err(invalid("invalid installed updater build identity"));
    }
    if let Some(origin) = &self.origin {
      origin.validate()?;
    }
    if let Some(channel) = self.tracked_channel
      && self.origin.as_ref().map(|origin| origin.channel.as_str()) != Some(channel.name())
    {
      return Err(invalid("tracked updater channel differs from its origin"));
    }
    Ok(())
  }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdaterSelections {
  schema_version: u16,
  entries: Vec<UpdaterSelection>,
}

impl UpdaterSelections {
  fn validate(&self) -> io::Result<()> {
    if self.schema_version != 1 || self.entries.len() > 2 {
      return Err(invalid(
        "unsupported or oversized updater selection metadata",
      ));
    }
    for entry in &self.entries {
      entry.validate()?;
    }
    if let [old, next] = self.entries.as_slice()
      && old.content_id == next.content_id
    {
      return Err(invalid("duplicate updater selection identity"));
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
  pub(super) development: BTreeMap<String, String>,
  pub(super) active_runtime: Option<RuntimeSelection>,
  pub(super) desktop: Option<DesktopSelection>,
  pub(super) updater: Option<String>,
  #[serde(skip)]
  pub(super) runtime_paths: PayloadPaths,
  #[serde(skip)]
  pub(super) updater_paths: PayloadPaths,
  #[serde(skip)]
  pub(super) desktop_paths: PathIndex,
}

#[derive(Debug, Clone, Default)]
pub(super) struct PayloadPaths {
  pub(super) target_first: PathIndex,
  pub(super) build_first: PathIndex,
  pub(super) object: PathIndex,
}

impl PayloadPaths {
  pub(super) fn location(&self, id: &str) -> Option<&str> {
    self
      .object
      .locations
      .get(id)
      .or_else(|| self.build_first.locations.get(id))
      .or_else(|| self.target_first.locations.get(id))
      .map(String::as_str)
  }

  pub(super) fn locations(&self) -> impl Iterator<Item = (&String, &String)> {
    self
      .target_first
      .locations
      .iter()
      .chain(&self.build_first.locations)
      .chain(&self.object.locations)
  }

  pub(super) fn reserves(&self, location: &str) -> bool {
    self.locations().any(|(_, existing)| {
      existing.eq_ignore_ascii_case(location)
        || existing
          .split_once('/')
          .is_some_and(|(parent, _)| parent.eq_ignore_ascii_case(location))
        || location
          .split_once('/')
          .is_some_and(|(parent, _)| existing.eq_ignore_ascii_case(parent))
    })
  }

  fn target(&self, id: &str) -> Option<&str> {
    if let Some(path) = self.build_first.locations.get(id) {
      return path.split_once('/').map(|(_, target)| target);
    }
    self
      .target_first
      .locations
      .get(id)?
      .split_once('/')
      .map(|(target, _)| target)
  }

  fn validate(
    &self,
    target_first: u16,
    build_first: Option<u16>,
    object: Option<u16>,
  ) -> io::Result<()> {
    self
      .target_first
      .validate(Some(target_first), Order::TargetFirst)?;
    self.build_first.validate(build_first, Order::BuildFirst)?;
    self.object.validate(object, Order::Object)?;
    let mut identities = BTreeSet::new();
    if self.locations().any(|(id, _)| !identities.insert(id)) {
      return Err(invalid(
        "duplicate installed payload identity across path indexes",
      ));
    }
    if !self.object.locations.is_empty() {
      let reserved: BTreeSet<_> = self
        .target_first
        .locations
        .values()
        .chain(self.build_first.locations.values())
        .filter_map(|location| {
          location
            .split_once('/')
            .map(|(parent, _)| parent.to_ascii_lowercase())
        })
        .collect();
      if self
        .object
        .locations
        .values()
        .any(|location| reserved.contains(&location.to_ascii_lowercase()))
      {
        return Err(invalid("updater object overlaps a legacy payload group"));
      }
    }
    Ok(())
  }
}

/// Preserve the original schema and path order for older executables that
/// acquire their own leases, while reserving new paths before publication.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PathIndex {
  schema_version: u16,
  pub(super) launcher_layout: Option<u16>,
  pub(super) locations: BTreeMap<String, String>,
}

impl Default for PathIndex {
  fn default() -> Self {
    Self {
      schema_version: 1,
      launcher_layout: None,
      locations: BTreeMap::new(),
    }
  }
}

#[derive(Clone, Copy)]
enum Order {
  TargetFirst,
  BuildFirst,
  Object,
}

impl PathIndex {
  pub(super) fn location(&self, id: &str) -> Option<&str> {
    self.locations.get(id).map(String::as_str)
  }

  pub(super) fn id_for_object(&self, name: &str) -> Option<&str> {
    self
      .locations
      .iter()
      .find_map(|(id, location)| (location == name).then_some(id.as_str()))
  }

  fn validate(&self, layout: Option<u16>, order: Order) -> io::Result<()> {
    if self.schema_version != 1
      || self.locations.len() > MAX_ENTRIES
      || self
        .launcher_layout
        .is_some_and(|version| Some(version) != layout)
    {
      return Err(invalid("unsupported or oversized payload path index"));
    }
    let mut occupied = BTreeSet::new();
    for (id, location) in &self.locations {
      let name = if matches!(order, Order::Object) {
        location.as_str()
      } else {
        let (first, second) = location
          .split_once('/')
          .ok_or_else(|| invalid("invalid installed payload location"))?;
        let (target, name) = if matches!(order, Order::TargetFirst) {
          (first, second)
        } else {
          (second, first)
        };
        crate::bundles::validate_target(target)?;
        name
      };
      validate_payload_label(name)?;
      if !filesystem::valid_id(id) || !occupied.insert(location.to_ascii_lowercase()) {
        return Err(invalid("invalid installed payload location"));
      }
    }
    Ok(())
  }
}

pub(super) fn validate_payload_label(name: &str) -> io::Result<()> {
  if !crate::installed_build_name::valid_label(name) {
    return Err(invalid("invalid installed payload location"));
  }
  Ok(())
}

impl Default for State {
  fn default() -> Self {
    Self {
      schema_version: 2,
      entries: Vec::new(),
      development: BTreeMap::new(),
      active_runtime: None,
      desktop: None,
      updater: None,
      runtime_paths: PayloadPaths::default(),
      updater_paths: PayloadPaths::default(),
      desktop_paths: PathIndex::default(),
    }
  }
}

impl State {
  fn validate(&self, store: &Installations) -> io::Result<()> {
    self.load_payloads(store, false).map(|_| ())
  }

  pub(super) fn inspect_payloads(
    &self,
    store: &Installations,
  ) -> io::Result<(Vec<Bundle>, Vec<String>)> {
    self.load_payloads(store, true)
  }

  fn validate_present_payloads(&self, store: &Installations) -> io::Result<()> {
    self.load_payloads(store, true).map(|_| ())
  }

  fn load_payloads(
    &self,
    store: &Installations,
    allow_missing: bool,
  ) -> io::Result<(Vec<Bundle>, Vec<String>)> {
    self.validate_index()?;
    let dev_ids = self.development.values().collect::<BTreeSet<_>>();
    let mut bundles = Vec::new();
    let mut missing = Vec::new();
    for (id, target) in self
      .entries
      .iter()
      .map(|entry| (&entry.bundle_id, &entry.target_triple))
      .collect::<BTreeMap<_, _>>()
    {
      let Some(bundle) = filesystem::load_available(store, self, id)? else {
        if !allow_missing {
          return Err(filesystem::missing_payload(id));
        }
        missing.push(id.clone());
        continue;
      };
      if bundle.manifest.target_triple != *target {
        return Err(invalid("catalog target differs from its immutable payload"));
      }
      if dev_ids.contains(id) && bundle.manifest.source != crate::bundles::Source::Local {
        return Err(invalid("development checkout requires a local build"));
      }
      bundles.push(bundle);
    }
    Ok((bundles, missing))
  }

  fn validate_index(&self) -> io::Result<()> {
    self
      .runtime_paths
      .validate(super::RUNTIME_PATH_LAYOUT_VERSION, None, None)?;
    self.updater_paths.validate(
      super::UPDATER_PATH_LAYOUT_VERSION,
      Some(super::UPDATER_BUILD_PATH_LAYOUT_VERSION),
      Some(super::UPDATER_OBJECT_PATH_LAYOUT_VERSION),
    )?;
    self.desktop_paths.validate(None, Order::Object)?;
    if self.schema_version != 2
      || self.entries.len() > MAX_ENTRIES
      || self.development.len() > MAX_ENTRIES
    {
      return Err(invalid("unsupported or oversized installation catalog"));
    }
    let mut unique = BTreeSet::new();
    let mut heads = BTreeSet::new();
    let mut publications = BTreeMap::new();
    let mut targets = BTreeMap::new();
    let mut dev_bundles = BTreeSet::new();
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
      if self
        .runtime_paths
        .target(&entry.bundle_id)
        .is_some_and(|target| target != entry.target_triple)
      {
        return Err(invalid(
          "catalog target differs from its runtime payload location",
        ));
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
      if origin.channel == "local" && crate::bundles::local_target(&entry.target_triple) {
        dev_bundles.insert(entry.bundle_id.as_str());
      }
    }
    for (checkout, id) in &self.development {
      let (checkout, target) = checkout
        .split_once('/')
        .ok_or_else(|| invalid("invalid development checkout registration"))?;
      validate_checkout_id(checkout)?;
      crate::bundles::validate_target(target)?;
      if !dev_bundles.contains(id.as_str())
        || targets
          .get(id)
          .is_none_or(|recorded| recorded.as_str() != target)
      {
        return Err(invalid(
          "development checkout has no recorded local native build",
        ));
      }
    }
    if let Some(active) = &self.active_runtime {
      active.origin.validate()?;
      if !targets
        .get(&active.bundle_id)
        .is_some_and(|target| crate::bundles::local_target(target))
      {
        return Err(invalid("runtime selection has no recorded native build"));
      }
    }
    if let Some(desktop) = &self.desktop {
      desktop.validate()?;
    }
    if self
      .updater
      .as_deref()
      .is_some_and(|id| !crate::bundles::valid_digest(id))
    {
      return Err(invalid("invalid updater selection"));
    }
    Ok(())
  }

  pub(super) fn select_runtime(
    &mut self,
    bundle: &Bundle,
    origin: Option<&CatalogOrigin>,
  ) -> io::Result<()> {
    let bundle_id = &bundle.manifest.bundle_id;
    let origin = origin
      .or_else(|| {
        self
          .active_runtime
          .as_ref()
          .filter(|selected| &selected.bundle_id == bundle_id)
          .map(|selected| &selected.origin)
      })
      .or_else(|| {
        self
          .entries
          .iter()
          .filter(|entry| &entry.bundle_id == bundle_id)
          .max_by_key(|entry| entry.current)
          .and_then(|entry| entry.origin.as_ref())
      })
      .ok_or_else(|| invalid("runtime selection lacks installation provenance"))?;
    self.active_runtime = Some(RuntimeSelection {
      bundle_id: bundle_id.clone(),
      origin: origin.clone(),
    });
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
    self.sort_entries();
  }

  #[cfg(target_os = "macos")]
  pub(super) fn replace_helper_memberships(
    &mut self,
    previous_id: &str,
    renewed_id: &str,
  ) -> io::Result<()> {
    for id in self.development.values_mut() {
      if id == previous_id {
        *id = renewed_id.into();
      }
    }
    let mut renewed_entries = Vec::new();
    for entry in self
      .entries
      .iter_mut()
      .filter(|entry| entry.bundle_id == previous_id)
    {
      let mut renewed = entry.clone();
      renewed.bundle_id = renewed_id.into();
      renewed_entries.push(renewed);
      entry.current = false;
    }
    for entry in renewed_entries {
      let channel = entry.origin.as_ref().map(|origin| &origin.channel);
      if let Some(existing) = self.entries.iter_mut().find(|existing| {
        existing.bundle_id == renewed_id
          && existing.origin.as_ref().map(|origin| &origin.channel) == channel
      }) {
        if existing.origin != entry.origin {
          return Err(invalid(
            "renewed helper already has different provenance in this channel",
          ));
        }
        // Retain any current channel membership already held by the replacement.
        existing.current |= entry.current;
      } else {
        self.entries.push(entry);
      }
    }
    self.sort_entries();
    self.validate_index()
  }

  fn sort_entries(&mut self) {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeSelection {
  pub(super) bundle_id: String,
  pub(super) origin: CatalogOrigin,
}

/// The source recorded by a desktop activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum DesktopSource {
  Channel { channel: String },
  Release { publication_id: String },
  PullRequest { number: u64 },
  Provided,
  Checkout,
}

/// A selected desktop payload and its source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopSelection {
  pub content_id: String,
  pub source: DesktopSource,
}

impl DesktopSelection {
  pub(super) fn validate(&self) -> io::Result<()> {
    if !crate::bundles::valid_digest(&self.content_id) {
      return Err(invalid("invalid desktop content identity"));
    }
    match &self.source {
      DesktopSource::Channel { channel } if !matches!(channel.as_str(), "stable" | "nightly") => {
        Err(invalid("invalid desktop channel"))
      }
      DesktopSource::Release { publication_id } => CatalogOrigin {
        channel: "release".into(),
        publication_id: Some(publication_id.clone()),
        built_at: None,
      }
      .validate(),
      DesktopSource::PullRequest { number: 0 } => {
        Err(invalid("invalid desktop pull request number"))
      }
      _ => Ok(()),
    }
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
  if !matches!(
    channel,
    "stable" | "nightly" | "local" | "provided" | "release"
  ) && !pr
  {
    return Err(invalid("invalid installation catalog channel"));
  }
  Ok(())
}

pub(super) fn validate_checkout_id(checkout: &str) -> io::Result<()> {
  if !crate::bundles::valid_digest(checkout) {
    return Err(invalid("invalid development checkout identity"));
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
  filesystem::path_exists(&store.state_root().join(CATALOG))
}

pub(super) fn read(store: &Installations) -> io::Result<State> {
  let state = read_index(store)?;
  state.validate_present_payloads(store)?;
  Ok(state)
}

pub(super) fn read_index(store: &Installations) -> io::Result<State> {
  let mut state: State = if let Some(state) = read_file(&store.state_root().join(CATALOG))? {
    state
  } else {
    if filesystem::has_payloads(store)? {
      return Err(invalid("managed payloads have no installation registry"));
    }
    State::default()
  };
  let root = store.state_root();
  state.runtime_paths.target_first = read_file(&root.join(RUNTIME_PATHS))?.unwrap_or_default();
  state.runtime_paths.build_first = read_file(&root.join(RUNTIME_BUILD_PATHS))?.unwrap_or_default();
  state.updater_paths.target_first = read_file(&root.join(UPDATER_PATHS))?.unwrap_or_default();
  state.updater_paths.build_first = read_file(&root.join(UPDATER_BUILD_PATHS))?.unwrap_or_default();
  state.updater_paths.object = read_file(&root.join(UPDATER_OBJECT_PATHS))?.unwrap_or_default();
  state.desktop_paths = read_file(&root.join(DESKTOP_OBJECT_PATHS))?.unwrap_or_default();
  state.validate_index()?;
  Ok(state)
}

pub(super) fn read_updater_selection(
  store: &Installations,
  active: Option<&str>,
) -> io::Result<Option<UpdaterSelection>> {
  let Some(active) = active else {
    return Ok(None);
  };
  let path = store.state_root().join(UPDATER_SELECTION);
  let Some(selections) = read_file::<UpdaterSelections>(&path)? else {
    return Ok(None);
  };
  selections.validate()?;
  Ok(
    selections
      .entries
      .into_iter()
      .find(|entry| entry.content_id == active),
  )
}

// Keep the previous identity readable until the unchanged registry switches
// its digest pointer, including when a process stops between the two commits.
pub(super) fn commit_updater_selection(
  store: &Installations,
  previous: Option<UpdaterSelection>,
  selection: &UpdaterSelection,
) -> io::Result<()> {
  let mut entries = Vec::with_capacity(2);
  if let Some(previous) = previous.filter(|previous| previous.content_id != selection.content_id) {
    entries.push(previous);
  }
  entries.push(selection.clone());
  let selections = UpdaterSelections {
    schema_version: 1,
    entries,
  };
  selections.validate()?;
  replace(&store.state_root().join(UPDATER_SELECTION), &selections)
}

pub(super) fn commit_runtime_paths(store: &Installations, paths: &PathIndex) -> io::Result<()> {
  paths.validate(Some(super::RUNTIME_PATH_LAYOUT_VERSION), Order::TargetFirst)?;
  replace(&store.state_root().join(RUNTIME_PATHS), paths)
}

pub(super) fn commit_updater_paths(store: &Installations, paths: &PathIndex) -> io::Result<()> {
  paths.validate(Some(super::UPDATER_PATH_LAYOUT_VERSION), Order::TargetFirst)?;
  replace(&store.state_root().join(UPDATER_PATHS), paths)
}

pub(super) fn commit_runtime_build_paths(
  store: &Installations,
  paths: &PathIndex,
) -> io::Result<()> {
  paths.validate(None, Order::BuildFirst)?;
  replace(&store.state_root().join(RUNTIME_BUILD_PATHS), paths)
}

pub(super) fn commit_updater_build_paths(
  store: &Installations,
  paths: &PathIndex,
) -> io::Result<()> {
  paths.validate(
    Some(super::UPDATER_BUILD_PATH_LAYOUT_VERSION),
    Order::BuildFirst,
  )?;
  replace(&store.state_root().join(UPDATER_BUILD_PATHS), paths)
}

pub(super) fn commit_updater_object_paths(
  store: &Installations,
  paths: &PathIndex,
) -> io::Result<()> {
  paths.validate(
    Some(super::UPDATER_OBJECT_PATH_LAYOUT_VERSION),
    Order::Object,
  )?;
  replace(&store.state_root().join(UPDATER_OBJECT_PATHS), paths)
}

pub(super) fn commit_desktop_paths(store: &Installations, paths: &PathIndex) -> io::Result<()> {
  paths.validate(None, Order::Object)?;
  replace(&store.state_root().join(DESKTOP_OBJECT_PATHS), paths)
}

pub(super) fn initialize(store: &Installations, state: &State) -> io::Result<()> {
  if !exists(store)? {
    commit_metadata(store, state)?;
  }
  Ok(())
}

pub(super) fn inventory(store: &Installations) -> io::Result<Vec<CatalogEntry>> {
  let state = read_index(store)?;
  state.validate(store)?;
  Ok(state.entries)
}

/// The locked writer publishes one snapshot after new immutable bytes are durable.
pub(super) fn commit(store: &Installations, state: &State) -> io::Result<()> {
  state.validate_present_payloads(store)?;
  commit_metadata(store, state)
}

/// The desktop and manager only change their own selection; they do not acquire
/// or alter runtime payloads. Callers hold the same profile-wide registry lock.
pub(super) fn commit_metadata(store: &Installations, state: &State) -> io::Result<()> {
  state.validate_index()?;
  replace(&store.state_root().join(CATALOG), state)
}

fn read_file<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<Option<T>> {
  match fs::symlink_metadata(path) {
    Ok(metadata) => filesystem::private_file(&metadata)?,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  }
  let file = match options().read(true).open(path) {
    Ok(file) => file,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
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
  validate_destination(path)?;
  let temporary =
    filesystem::Temporary(path.with_file_name(format!(".catalog-next-{}", uuid::Uuid::new_v4())));
  let mut file = options().write(true).create_new(true).open(&temporary.0)?;
  file.write_all(&bytes)?;
  file.sync_all()?;
  drop(file);
  fs::rename(&temporary.0, path)?;
  sync_directory(path.parent().unwrap())
}

fn validate_destination(path: &Path) -> io::Result<()> {
  match fs::symlink_metadata(path) {
    Ok(metadata) => filesystem::private_file(&metadata),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error),
  }
}

#[cfg(test)]
mod tests;
