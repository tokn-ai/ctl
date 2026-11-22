//! One active complete installation with stable commands and bounded retention.
//!
//! Installing changes only the next executable launch. Existing processes hold
//! leases on immutable payloads and continue using their original installation.

mod catalog;
mod filesystem;
mod leases;
#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, windows))]
mod windows_tests;

use crate::bundles::{Bundle, Manifest};
use crate::component::ComponentBuildInfo;
use serde::Serialize;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

pub use catalog::{CatalogEntry, CatalogOrigin, DesktopSelection, DesktopSource};
pub use leases::{Lease, lease_current_executable};

/// Local updater capability for the installed payload layout.
pub const DEVELOPMENT_LAYOUT_VERSION: u16 = 6;
const RUNTIME_PATH_LAYOUT_VERSION: u16 = 3;
const UPDATER_PATH_LAYOUT_VERSION: u16 = 4;
const UPDATER_BUILD_PATH_LAYOUT_VERSION: u16 = 5;

#[cfg(test)]
fn manager_build_fixture() -> ComponentBuildInfo {
  ComponentBuildInfo {
    version: "0.1.2".into(),
    source_revision: Some("a".repeat(40)),
    source_fingerprint: "b".repeat(64),
    dirty: false,
  }
}

/// Complete installations for one account, with one native execution selection.
/// Catalogued foreign targets are retained for deployment without execution.
#[derive(Debug, Clone)]
pub struct Installations {
  root: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PruneResult {
  pub removed: Vec<String>,
  pub retained_in_use: Vec<String>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub missing_builds: Vec<String>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub forgotten_missing_builds: Vec<String>,
}

/// Recorded installation metadata with available payloads and missing IDs.
#[derive(Debug, Default)]
pub struct Inspection {
  pub entries: Vec<CatalogEntry>,
  pub active: Option<String>,
  pub active_origin: Option<CatalogOrigin>,
  pub previous: Option<String>,
  pub bundles: Vec<Bundle>,
  pub missing: Vec<String>,
}

impl Installations {
  /// Merely constructs paths for the selected installation store.
  #[must_use]
  pub fn new(home: &Path) -> Self {
    Self {
      root: crate::paths::directory_for_account(home),
    }
  }

  fn from_executable_root(root: &Path) -> Option<Self> {
    if let Some(selected) = crate::paths::data_override()
      && selected.canonicalize().ok().as_deref() == Some(root)
    {
      return Some(Self { root: selected });
    }
    if !root.ends_with(".tokn/ctl") {
      return None;
    }
    Some(Self {
      root: root.to_owned(),
    })
  }

  #[must_use]
  pub fn root(&self) -> &Path {
    &self.root
  }

  /// Root of all updater-managed state and payloads for this profile.
  #[must_use]
  pub fn updater_root(&self) -> PathBuf {
    crate::paths::updater_directory_for_profile(&self.root)
  }

  fn state_root(&self) -> PathBuf {
    self.updater_root().join("state")
  }

  fn runtime_root(&self) -> PathBuf {
    self.updater_root().join("store/runtime")
  }

  fn manager_root(&self) -> PathBuf {
    self.updater_root().join("store/updater")
  }

  fn desktop_root(&self) -> PathBuf {
    self.updater_root().join("store/desktop")
  }

  fn staging_root(&self) -> PathBuf {
    self.updater_root().join("staging")
  }

  fn runtime_leases(&self) -> PathBuf {
    self.state_root().join("leases/runtime")
  }

  fn manager_leases(&self) -> PathBuf {
    self.state_root().join("leases/updater")
  }

  fn desktop_leases(&self) -> PathBuf {
    self.state_root().join("leases/desktop")
  }

  /// Returns the active runtime source, including when its bytes are missing.
  ///
  /// # Errors
  /// Reports unsafe storage or invalid registry metadata.
  pub fn active_origin(&self) -> io::Result<Option<CatalogOrigin>> {
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    Ok(
      catalog::read_index(self)?
        .active_runtime
        .map(|selection| selection.origin),
    )
  }

  /// Reads the authoritative desktop selection.
  ///
  /// # Errors
  /// Reports unsafe storage or invalid registry metadata.
  pub fn desktop_selection(&self) -> io::Result<Option<DesktopSelection>> {
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    Ok(catalog::read_index(self)?.desktop)
  }

  /// Publishes a staged desktop bundle and its selection under the registry lock.
  ///
  /// # Errors
  /// Reports an invalid selection, a concurrent update, unsafe directories,
  /// publication failures, or a failed registry commit.
  pub fn publish_desktop<T>(
    &self,
    expected: Option<&DesktopSelection>,
    selection: &DesktopSelection,
    publish: impl FnOnce() -> io::Result<T>,
  ) -> io::Result<T> {
    selection.validate()?;
    filesystem::ensure_root(self)?;
    let _lock = filesystem::Lock::acquire(self)?;
    let mut state = catalog::read_index(self)?;
    if state.desktop.as_ref() != expected {
      return Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "desktop selection changed during preparation",
      ));
    }
    catalog::initialize(self, &state)?;
    filesystem::ensure_directory(&self.desktop_root())?;
    let published = publish()?;
    filesystem::directory(&self.desktop_root().join(&selection.content_id))?;
    state.desktop = Some(selection.clone());
    catalog::commit_metadata(self, &state)?;
    Ok(published)
  }

  /// Returns the selected updater's content identity.
  ///
  /// # Errors
  /// Reports unsafe storage or invalid registry metadata.
  pub fn active_manager_id(&self) -> io::Result<Option<String>> {
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    Ok(catalog::read_index(self)?.updater)
  }

  /// Reads the recorded complete active payload through its canonical selection.
  ///
  /// # Errors
  /// Reports unsafe metadata, unavailable bytes or damaged payloads.
  pub fn active(&self) -> io::Result<Option<Bundle>> {
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    self.recorded_selection()
  }

  /// Reads the active identity and its verified payload, when available.
  ///
  /// # Errors
  /// Reports unsafe metadata or an invalid or incomplete payload.
  pub fn active_selection(&self) -> io::Result<(Option<String>, Option<Bundle>)> {
    if !filesystem::root_exists(self)? {
      return Ok((None, None));
    }
    let _lock = filesystem::Lock::acquire(self)?;
    let state = catalog::read_index(self)?;
    self.load_selection(&state)
  }

  /// Resolves the active installation and keeps it available for a new process.
  ///
  /// # Errors
  /// Reports damaged installations or unavailable storage or leases.
  pub fn active_and_lease(&self) -> io::Result<Option<(Bundle, Lease)>> {
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    let Some(bundle) = self.recorded_selection()? else {
      return Ok(None);
    };
    let lease = leases::retain(&self.runtime_leases(), &bundle.manifest.bundle_id)?;
    Ok(Some((bundle, lease)))
  }

  /// Publishes a verified updater and selects it for future invocations.
  /// Existing invocations continue using their leased updater payloads.
  ///
  /// # Errors
  /// Reports unsafe storage, damaged aliases or collisions with other commands.
  pub fn install_manager(
    &self,
    updater: &[u8],
    build: &ComponentBuildInfo,
    origin: Option<&CatalogOrigin>,
  ) -> io::Result<()> {
    filesystem::install_manager(self, updater, build, origin)
  }

  /// Reads retained updater bytes after verifying their exact digest.
  ///
  /// # Errors
  /// Reports invalid identities, unsafe storage, or a damaged present payload.
  pub fn manager_bytes(&self, id: &str) -> io::Result<Option<Vec<u8>>> {
    if !filesystem::valid_id(id) {
      return Err(filesystem::invalid("invalid updater identity"));
    }
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    let state = catalog::read_index(self)?;
    filesystem::read_manager(self, &state.updater_paths, id)
      .map(|payload| payload.map(|(_, bytes)| bytes))
  }

  /// Resolves and pins the active updater for a new invocation.
  ///
  /// # Errors
  /// Reports unsafe storage, damaged updater content or unavailable leases.
  pub fn manager_and_lease(&self) -> io::Result<Option<(PathBuf, Lease)>> {
    filesystem::manager_and_lease(self)
  }

  /// Returns the previous selection field retained by status clients.
  ///
  /// # Errors
  /// Reports unsafe or invalid registry metadata.
  pub fn previous(&self) -> io::Result<Option<Bundle>> {
    if filesystem::root_exists(self)? {
      let _lock = filesystem::Lock::acquire(self)?;
      catalog::read_index(self)?;
    }
    Ok(None)
  }

  fn recorded_selection(&self) -> io::Result<Option<Bundle>> {
    let state = catalog::read_index(self)?;
    let (id, bundle) = self.load_selection(&state)?;
    match (id, bundle) {
      (Some(id), None) => Err(filesystem::missing_payload(&id)),
      (_, bundle) => Ok(bundle),
    }
  }

  fn selection_entry(state: &catalog::State) -> Option<&CatalogEntry> {
    state
      .active_runtime
      .as_ref()
      .and_then(|selection| catalog_entry(state, &selection.bundle_id))
  }

  fn load_selection(&self, state: &catalog::State) -> io::Result<(Option<String>, Option<Bundle>)> {
    let Some(entry) = Self::selection_entry(state) else {
      return Ok((None, None));
    };
    let bundle = self.load_entry(state, entry)?;
    if let Some(bundle) = &bundle {
      filesystem::validate_native(bundle)?;
    }
    Ok((Some(entry.bundle_id.clone()), bundle))
  }

  /// Opens one immutable installation by its content identity.
  ///
  /// # Errors
  /// Reports missing content distinctly from unsafe or corrupt content.
  pub fn get(&self, bundle_id: &str) -> io::Result<Bundle> {
    if !filesystem::root_exists(self)? {
      return Err(io::Error::new(
        io::ErrorKind::NotFound,
        "installation store is absent",
      ));
    }
    let _lock = filesystem::Lock::acquire(self)?;
    self.get_recorded(bundle_id)
  }

  fn get_recorded(&self, bundle_id: &str) -> io::Result<Bundle> {
    let state = catalog::read_index(self)?;
    let entry = catalog_entry(&state, bundle_id).ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::NotFound,
        "installation is not recorded in the catalog",
      )
    })?;
    self
      .load_entry(&state, entry)?
      .ok_or_else(|| filesystem::missing_payload(bundle_id))
  }

  /// Opens the verified payload for an installed identity if it is present.
  ///
  /// # Errors
  /// Reports unsafe or corrupt storage, including an incomplete payload directory.
  pub fn get_available(&self, bundle_id: &str) -> io::Result<Option<Bundle>> {
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    let state = catalog::read_index(self)?;
    let Some(entry) = catalog_entry(&state, bundle_id) else {
      return Ok(None);
    };
    self.load_entry(&state, entry)
  }

  fn load_entry(&self, state: &catalog::State, entry: &CatalogEntry) -> io::Result<Option<Bundle>> {
    let bundle = filesystem::load_available(self, state, &entry.bundle_id)?;
    if bundle
      .as_ref()
      .is_some_and(|bundle| bundle.manifest.target_triple != entry.target_triple)
    {
      return Err(filesystem::invalid(
        "catalog target differs from its immutable payload",
      ));
    }
    Ok(bundle)
  }

  /// Lists verified installations without changing the active build.
  ///
  /// # Errors
  /// Reports unsafe or corrupt entries rather than hiding them from inspection.
  pub fn list(&self) -> io::Result<Vec<Bundle>> {
    if !filesystem::root_exists(self)? {
      return Ok(Vec::new());
    }
    let _lock = filesystem::Lock::acquire(self)?;
    self.list_recorded()
  }

  fn list_recorded(&self) -> io::Result<Vec<Bundle>> {
    let state = catalog::read_index(self)?;
    state
      .entries
      .iter()
      .map(|entry| (&entry.bundle_id, &entry.target_triple))
      .collect::<std::collections::BTreeMap<_, _>>()
      .into_iter()
      .map(|(id, target)| {
        let bundle = filesystem::load(self, &state, id)?;
        if bundle.manifest.target_triple != *target {
          return Err(filesystem::invalid(
            "catalog target differs from its immutable payload",
          ));
        }
        Ok(bundle)
      })
      .collect()
  }

  /// Runs maintenance with verified manifests and missing catalog entries while
  /// holding the installation lock for an existing store. With no installation
  /// store, the callback receives empty slices.
  ///
  /// The callback must not mutate the installation store or acquire its lock
  /// again. Cache locks must be acquired inside the callback, after this lock.
  ///
  /// # Errors
  /// Reports unsafe storage, recovery or payload verification failures, and
  /// forwards any error returned by the callback.
  pub fn with_locked_manifests<T>(
    &self,
    operation: impl FnOnce(&[Manifest], &[CatalogEntry]) -> io::Result<T>,
  ) -> io::Result<T> {
    if !filesystem::root_exists(self)? {
      return operation(&[], &[]);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    let state = catalog::read_index(self)?;
    let (bundles, missing) = state.inspect_payloads(self)?;
    let manifests = bundles
      .into_iter()
      .map(|bundle| bundle.manifest)
      .collect::<Vec<_>>();
    let missing_entries = state
      .entries
      .into_iter()
      .filter(|entry| missing.binary_search(&entry.bundle_id).is_ok())
      .collect::<Vec<_>>();
    operation(&manifests, &missing_entries)
  }

  /// Lists recorded installations and their channel provenance.
  ///
  /// # Errors
  /// Reports unsafe storage, damaged catalog entries or modified payloads.
  pub fn catalog(&self) -> io::Result<Vec<CatalogEntry>> {
    if !filesystem::root_exists(self)? {
      return Ok(Vec::new());
    }
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::inventory(self)
  }

  /// Reads recorded provenance for an update snapshot without opening payloads.
  ///
  /// # Errors
  /// Reports unsafe storage or invalid catalog metadata.
  pub fn catalog_index(&self) -> io::Result<Vec<CatalogEntry>> {
    if !filesystem::root_exists(self)? {
      return Ok(Vec::new());
    }
    let _lock = filesystem::Lock::acquire(self)?;
    Ok(catalog::read_index(self)?.entries)
  }

  /// Inspects each registered payload and reports absent installations by ID.
  ///
  /// # Errors
  /// Reports unsafe storage or invalid metadata and payloads.
  pub fn inspect(&self) -> io::Result<Inspection> {
    self.inspect_recorded(true)
  }

  /// Verifies the selected payloads and checks remaining entries for absence.
  ///
  /// # Errors
  /// Reports unsafe storage, invalid metadata or invalid selected payloads.
  pub fn inspect_status(&self) -> io::Result<Inspection> {
    self.inspect_recorded(false)
  }

  fn inspect_recorded(&self, full: bool) -> io::Result<Inspection> {
    if !filesystem::root_exists(self)? {
      return Ok(Inspection::default());
    }
    let _lock = filesystem::Lock::acquire(self)?;
    let state = catalog::read_index(self)?;
    let active = Self::selection_entry(&state);
    let (bundles, missing) = if full {
      state.inspect_payloads(self)?
    } else {
      let bundle = active
        .map(|entry| self.load_entry(&state, entry))
        .transpose()?
        .flatten();
      let bundles = bundle.into_iter().collect();
      let ids: BTreeSet<_> = state.entries.iter().map(|entry| &entry.bundle_id).collect();
      let mut missing = Vec::new();
      for id in ids {
        if filesystem::payload_dir(self, &state, id)?.is_none() {
          missing.push(id.clone());
        }
      }
      (bundles, missing)
    };
    Ok(Inspection {
      active: active.map(|entry| entry.bundle_id.clone()),
      previous: None,
      active_origin: state
        .active_runtime
        .as_ref()
        .map(|selected| selected.origin.clone()),
      entries: state.entries,
      bundles,
      missing,
    })
  }

  /// Reads the current installed build for a hashed local checkout path.
  ///
  /// # Errors
  /// Reports an invalid checkout ID, unsafe storage or catalog corruption.
  pub fn development_head(&self, checkout_id: &str) -> io::Result<Option<String>> {
    catalog::validate_checkout_id(checkout_id)?;
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    let state = catalog::read_index(self)?;
    let target = crate::paths::native_target();
    if let Some(exact) = state.development.get(&format!("{checkout_id}/{target}")) {
      return Ok(Some(exact.clone()));
    }
    Ok(state.development.iter().find_map(|(key, id)| {
      let (checkout, target) = key.split_once('/')?;
      (checkout == checkout_id && crate::bundles::local_target(target)).then(|| id.clone())
    }))
  }

  /// Checks the registry before transfer, allowing missing payloads for repair.
  ///
  /// # Errors
  /// Reports unsafe metadata or damaged present payloads.
  pub fn prepare_installation(&self) -> io::Result<()> {
    if filesystem::root_exists(self)? {
      let _lock = filesystem::Lock::acquire(self)?;
      catalog::read(self)?;
    }
    Ok(())
  }

  /// Checks whether a checkout build is retained by the registry.
  ///
  /// # Errors
  /// Reports unsafe or unreadable metadata.
  pub fn has_development_heads(&self) -> io::Result<bool> {
    if !filesystem::root_exists(self)? {
      return Ok(false);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    Ok(!catalog::read_index(self)?.development.is_empty())
  }

  /// Opens the retained current component for a channel and target.
  ///
  /// # Errors
  /// Reports invalid selectors, corrupt metadata or modified payloads.
  pub fn component(&self, channel: &str, target: &str) -> io::Result<Option<Bundle>> {
    catalog::validate_channel(channel)?;
    crate::bundles::validate_target(target)?;
    self
      .catalog()?
      .into_iter()
      .find(|entry| {
        entry.current
          && entry.target_triple == target
          && entry
            .origin
            .as_ref()
            .is_some_and(|origin| origin.channel == channel)
      })
      .map(|entry| self.get(&entry.bundle_id))
      .transpose()
  }

  /// Installs one complete publication's target set and optionally activates its
  /// native payload. Other channels and running processes remain available.
  ///
  /// # Errors
  /// Rejects incomplete payloads, mixed source builds, duplicate targets, invalid
  /// provenance or unsafe storage. Activation prefers the exact native target
  /// and otherwise requires exactly one locally runnable target.
  pub fn install_channel(
    &self,
    bundles: &[Bundle],
    origin: &CatalogOrigin,
    activate_native: bool,
  ) -> io::Result<Vec<Bundle>> {
    self.install_channel_with_snapshot(bundles, origin, activate_native, None)
  }

  /// Installs a prepared publication only if its channel heads still match the
  /// snapshot taken before preparation. Other channels may change independently.
  /// The snapshot contains only current entries for `origin.channel`.
  ///
  /// # Errors
  /// Reports the same failures as [`Self::install_channel`], or `WouldBlock` when
  /// another operation changed this channel before it acquired its lock.
  pub fn install_channel_if_unchanged(
    &self,
    bundles: &[Bundle],
    origin: &CatalogOrigin,
    activate_native: bool,
    expected_heads: &[CatalogEntry],
  ) -> io::Result<Vec<Bundle>> {
    self.install_channel_with_snapshot(bundles, origin, activate_native, Some(expected_heads))
  }

  fn install_channel_with_snapshot(
    &self,
    bundles: &[Bundle],
    origin: &CatalogOrigin,
    activate_native: bool,
    expected_heads: Option<&[CatalogEntry]>,
  ) -> io::Result<Vec<Bundle>> {
    origin.validate()?;
    if bundles.is_empty() {
      return Err(filesystem::invalid(
        "a channel installation requires a target",
      ));
    }
    let source_build = &bundles[0]
      .manifest
      .components
      .get("ctl-agent")
      .ok_or_else(|| filesystem::invalid("a channel requires complete component builds"))?
      .build;
    let mut targets = BTreeSet::new();
    let mut compatible = Vec::new();
    let mut exact = None;
    for (index, bundle) in bundles.iter().enumerate() {
      filesystem::validate_complete(bundle)?;
      if &bundle.manifest.components["ctl-agent"].build != source_build {
        return Err(filesystem::invalid(
          "channel targets must share one source build",
        ));
      }
      if !targets.insert(&bundle.manifest.target_triple) {
        return Err(filesystem::invalid("a channel contains duplicate targets"));
      }
      if activate_native && crate::bundles::local_target(&bundle.manifest.target_triple) {
        compatible.push(index);
        if bundle.manifest.target_triple == crate::paths::native_target() {
          exact = Some(index);
        }
      }
      // Validate every target before publishing, without retaining every large
      // archive in memory at once. Publication verifies each target again.
      drop(bundle.read_files()?);
    }
    let native = if !activate_native {
      None
    } else if let Some(exact) = exact {
      Some(exact)
    } else if let [compatible] = compatible.as_slice() {
      Some(*compatible)
    } else {
      return Err(filesystem::invalid(
        "the channel requires one unambiguous locally runnable target",
      ));
    };
    filesystem::ensure_root(self)?;
    let _lock = filesystem::Lock::acquire(self)?;
    let mut state = catalog::read(self)?;
    if let Some(expected_heads) = expected_heads {
      verify_channel_heads(&state, &origin.channel, expected_heads)?;
    }
    catalog::initialize(self, &state)?;
    let installed = bundles
      .iter()
      .map(|bundle| filesystem::publish(self, &mut state, bundle, None, Some(origin)))
      .collect::<io::Result<Vec<_>>>()?;
    state.register(&installed, origin, false);
    if let Some(index) = native {
      filesystem::prepare_activation(self)?;
      state.select_runtime(&installed[index], Some(origin))?;
    }
    catalog::commit(self, &state)?;
    Ok(installed)
  }

  /// Unregisters a channel target. Active and leased payloads survive
  /// pruning and retain their provenance until their payloads are removed.
  /// Call [`Self::prune`] separately to clean up unreferenced payloads.
  ///
  /// # Errors
  /// Reports invalid selectors, unsafe metadata or failed catalog publication.
  pub fn remove_component(&self, channel: &str, target: &str) -> io::Result<bool> {
    catalog::validate_channel(channel)?;
    crate::bundles::validate_target(target)?;
    if !filesystem::root_exists(self)? {
      return Ok(false);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    let mut state = catalog::read(self)?;
    let mut removed = false;
    for entry in &mut state.entries {
      if entry.current
        && entry.target_triple == target
        && entry
          .origin
          .as_ref()
          .is_some_and(|origin| origin.channel == channel)
      {
        entry.current = false;
        removed = true;
      }
    }
    if channel == "local" {
      let checkout_builds = state
        .entries
        .iter()
        .filter(|entry| entry.target_triple == target)
        .map(|entry| entry.bundle_id.as_str())
        .collect::<BTreeSet<_>>();
      let old_count = state.development.len();
      state
        .development
        .retain(|_, id| !checkout_builds.contains(id.as_str()));
      removed |= state.development.len() != old_count;
    }
    if !removed {
      return Ok(false);
    }
    catalog::commit(self, &state)?;
    Ok(true)
  }

  /// Verifies, publishes and atomically activates one complete native build.
  /// Running services keep their existing leases.
  ///
  /// # Errors
  /// Rejects incomplete or foreign builds, unsafe storage, changed bytes and
  /// command collisions. A pending transaction is recovered before a new one.
  pub fn activate(&self, bundle: &Bundle) -> io::Result<Bundle> {
    self
      .activate_with(bundle, None, None, |_| Ok(()), |_| Ok(()))
      .map(|(installed, ())| installed)
  }

  /// Activates and retains a build in one transaction, so another updater
  /// cannot prune it before its launcher obtains a process lease.
  ///
  /// # Errors
  /// Reports activation errors or an unavailable lease before switching builds.
  pub fn activate_and_lease(&self, bundle: &Bundle) -> io::Result<(Bundle, Lease)> {
    self.activate_with(
      bundle,
      None,
      None,
      |_| Ok(()),
      |installed| leases::retain(&self.runtime_leases(), &installed.manifest.bundle_id),
    )
  }

  /// Activates a native build and records its channel provenance.
  ///
  /// # Errors
  /// Reports the same validation and storage errors as [`Self::activate`].
  pub fn activate_with_origin(
    &self,
    bundle: &Bundle,
    origin: &CatalogOrigin,
  ) -> io::Result<Bundle> {
    self
      .activate_with(bundle, Some(origin), None, |_| Ok(()), |_| Ok(()))
      .map(|(bundle, ())| bundle)
  }

  /// Activates, catalogues and leases a native build in one transaction.
  ///
  /// # Errors
  /// Reports activation, provenance or process-lease failures.
  pub fn activate_and_lease_with_origin(
    &self,
    bundle: &Bundle,
    origin: &CatalogOrigin,
  ) -> io::Result<(Bundle, Lease)> {
    self.activate_with(
      bundle,
      Some(origin),
      None,
      |_| Ok(()),
      |installed| leases::retain(&self.runtime_leases(), &installed.manifest.bundle_id),
    )
  }

  /// Activates and leases a native build only if its prepared channel snapshot
  /// still matches. The snapshot contains only current entries for its channel.
  ///
  /// # Errors
  /// Reports activation, provenance or lease failures, or `WouldBlock` if channel
  /// components changed before the installation lock was acquired.
  pub fn activate_and_lease_with_origin_if_unchanged(
    &self,
    bundle: &Bundle,
    origin: &CatalogOrigin,
    expected_heads: &[CatalogEntry],
  ) -> io::Result<(Bundle, Lease)> {
    self.activate_with(
      bundle,
      Some(origin),
      Some(expected_heads),
      |_| Ok(()),
      |installed| leases::retain(&self.runtime_leases(), &installed.manifest.bundle_id),
    )
  }

  /// Activates a local checkout's next build after comparing its prepared head
  /// under the installation lock. Each checkout retains one registered build.
  ///
  /// # Errors
  /// Rejects invalid or non-native development builds, unsafe storage, and
  /// returns `WouldBlock` when this checkout changed during preparation.
  pub fn activate_development(
    &self,
    bundle: &Bundle,
    origin: &CatalogOrigin,
    checkout_id: &str,
    expected_head: Option<&str>,
  ) -> io::Result<(Bundle, Lease)> {
    catalog::validate_checkout_id(checkout_id)?;
    if origin.channel != "local" || bundle.manifest.source != crate::bundles::Source::Local {
      return Err(filesystem::invalid(
        "development checkout requires a local build",
      ));
    }
    let checkout_key = format!("{checkout_id}/{}", bundle.manifest.target_triple);
    self.activate_with(
      bundle,
      Some(origin),
      None,
      |state| {
        if state.development.get(&checkout_key).map(String::as_str) != expected_head {
          return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "development checkout was updated during preparation; retry the build",
          ));
        }
        self.verify_development_siblings(state, bundle, origin)?;
        state
          .development
          .insert(checkout_key, bundle.manifest.bundle_id.clone());
        Ok(())
      },
      |installed| leases::retain(&self.runtime_leases(), &installed.manifest.bundle_id),
    )
  }

  fn verify_development_siblings(
    &self,
    state: &catalog::State,
    bundle: &Bundle,
    origin: &CatalogOrigin,
  ) -> io::Result<()> {
    let target = &bundle.manifest.target_triple;
    let build = &bundle.manifest.components["ctl-agent"].build;
    for entry in state.entries.iter().filter(|entry| {
      entry.current
        && &entry.target_triple != target
        && entry
          .origin
          .as_ref()
          .is_some_and(|entry| entry.channel == origin.channel)
    }) {
      if origin.publication_id.is_none()
        || entry
          .origin
          .as_ref()
          .and_then(|entry| entry.publication_id.as_ref())
          != origin.publication_id.as_ref()
        || self
          .load_entry(state, entry)?
          .is_none_or(|sibling| &sibling.manifest.components["ctl-agent"].build != build)
      {
        return Err(io::Error::new(
          io::ErrorKind::InvalidInput,
          "this development build cannot replace other targets in the local channel; remove the extra component registrations first",
        ));
      }
    }
    Ok(())
  }

  /// Activates a verified re-signing of the active local macOS build. Carries
  /// its catalog provenance and checkout registrations forward.
  /// The caller verifies the replacement's platform signature and identity.
  ///
  /// # Errors
  /// Rejects changes outside the helper package, conflicting catalog records,
  /// an active selection changed during signing, or failed storage operations.
  #[cfg(target_os = "macos")]
  pub fn activate_renewed_helper(
    &self,
    bundle: &Bundle,
    expected_active: &str,
  ) -> io::Result<(Bundle, Lease)> {
    filesystem::validate_native(bundle)?;
    let files = bundle.read_files()?;
    if !filesystem::root_exists(self)? {
      return Err(helper_selection_changed());
    }
    let _lock = filesystem::Lock::acquire(self)?;
    let active = self
      .recorded_selection()?
      .filter(|active| active.manifest.bundle_id == expected_active)
      .ok_or_else(helper_selection_changed)?;
    validate_helper_renewal(&active.manifest, &bundle.manifest)?;
    if active.manifest.bundle_id == bundle.manifest.bundle_id {
      let lease = leases::retain(&self.runtime_leases(), &active.manifest.bundle_id)?;
      return Ok((active, lease));
    }

    let mut state = catalog::read(self)?;
    let origin = state
      .active_runtime
      .as_ref()
      .map(|selected| selected.origin.clone());
    state.replace_helper_memberships(expected_active, &bundle.manifest.bundle_id)?;
    let installed = filesystem::publish(self, &mut state, bundle, Some(&files), origin.as_ref())?;
    let lease = leases::retain(&self.runtime_leases(), &installed.manifest.bundle_id)?;
    if let Some(selected) = &mut state.active_runtime {
      selected.bundle_id.clone_from(&installed.manifest.bundle_id);
    }
    filesystem::prepare_activation(self)?;
    catalog::commit(self, &state)?;
    Ok((installed, lease))
  }

  fn activate_with<T>(
    &self,
    bundle: &Bundle,
    origin: Option<&CatalogOrigin>,
    expected_heads: Option<&[CatalogEntry]>,
    update: impl FnOnce(&mut catalog::State) -> io::Result<()>,
    prepare: impl FnOnce(&Bundle) -> io::Result<T>,
  ) -> io::Result<(Bundle, T)> {
    filesystem::validate_native(bundle)?;
    if let Some(origin) = origin {
      origin.validate()?;
    }
    let files = bundle.read_files()?;
    filesystem::ensure_root(self)?;
    let _lock = filesystem::Lock::acquire(self)?;
    let mut state = catalog::read(self)?;
    if let (Some(origin), Some(expected_heads)) = (origin, expected_heads) {
      verify_channel_heads(&state, &origin.channel, expected_heads)?;
    }
    let inferred_origin = if origin.is_none()
      && !state
        .entries
        .iter()
        .any(|entry| entry.bundle_id == bundle.manifest.bundle_id)
    {
      Some(self.inferred_origin(bundle, &state)?)
    } else {
      None
    };
    let path_origin = origin
      .or(inferred_origin.as_ref())
      .or_else(|| {
        state
          .active_runtime
          .as_ref()
          .filter(|selected| selected.bundle_id == bundle.manifest.bundle_id)
          .map(|selected| &selected.origin)
      })
      .or_else(|| {
        catalog_entry(&state, &bundle.manifest.bundle_id).and_then(|entry| entry.origin.as_ref())
      })
      .cloned();
    catalog::initialize(self, &state)?;
    update(&mut state)?;
    let installed =
      filesystem::publish(self, &mut state, bundle, Some(&files), path_origin.as_ref())?;
    let retained = prepare(&installed)?;
    if let Some(origin) = origin {
      state.register(std::slice::from_ref(&installed), origin, true);
    } else if let Some(origin) = &inferred_origin {
      state.register(std::slice::from_ref(&installed), origin, true);
    }
    filesystem::prepare_activation(self)?;
    state.select_runtime(&installed, origin.or(inferred_origin.as_ref()))?;
    catalog::commit(self, &state)?;
    Ok((installed, retained))
  }

  fn inferred_origin(&self, bundle: &Bundle, state: &catalog::State) -> io::Result<CatalogOrigin> {
    let origin = CatalogOrigin {
      channel: "provided".into(),
      publication_id: Some(
        bundle
          .manifest
          .distribution_id
          .as_ref()
          .unwrap_or(&bundle.manifest.bundle_id)
          .clone(),
      ),
      built_at: None,
    };
    for entry in state.entries.iter().filter(|entry| {
      entry.current
        && entry.target_triple != bundle.manifest.target_triple
        && entry
          .origin
          .as_ref()
          .is_some_and(|existing| existing.channel == origin.channel)
    }) {
      if entry
        .origin
        .as_ref()
        .and_then(|existing| existing.publication_id.as_ref())
        != origin.publication_id.as_ref()
        || self.load_entry(state, entry)?.is_none_or(|sibling| {
          sibling.manifest.components["ctl-agent"].build
            != bundle.manifest.components["ctl-agent"].build
        })
      {
        return Err(io::Error::new(
          io::ErrorKind::InvalidInput,
          "inferred provided installation cannot replace other target components; install the complete matching channel cohort",
        ));
      }
    }
    Ok(origin)
  }

  /// Removes unused present builds and forgets registrations whose payloads
  /// are missing, clearing a missing active selection.
  ///
  /// # Errors
  /// Unsafe entries or unreadable leases stop pruning without guessing whether
  /// a process is alive. No PID or time-based ownership heuristic is used.
  pub fn prune(&self) -> io::Result<PruneResult> {
    self.prune_all(true)
  }

  /// Removes unused present builds while retaining repairable missing records.
  ///
  /// # Errors
  /// Reports unsafe storage, damaged existing payloads or unreadable leases.
  pub fn prune_unused(&self) -> io::Result<PruneResult> {
    self.prune_with_options(false, false)
  }

  /// Prunes all package stores while respecting active selections and leases.
  /// The caller must serialize desktop launch-entry refresh with this operation.
  ///
  /// # Errors
  /// Reports unsafe storage, damaged existing payloads or unreadable leases.
  pub fn prune_all(&self, forget_missing: bool) -> io::Result<PruneResult> {
    self.prune_with_options(forget_missing, true)
  }

  fn prune_with_options(&self, forget_missing: bool, desktop: bool) -> io::Result<PruneResult> {
    if !filesystem::root_exists(self)? {
      return Ok(PruneResult::default());
    }
    let _lock = filesystem::Lock::acquire(self)?;
    self.prune_locked(forget_missing, desktop)
  }

  fn prune_locked(&self, forget_missing: bool, desktop: bool) -> io::Result<PruneResult> {
    let mut state = catalog::read_index(self)?;
    let (_, missing_builds) = state.inspect_payloads(self)?;
    let mut retained = BTreeSet::new();
    let active_id = Self::selection_entry(&state).map(|entry| entry.bundle_id.clone());
    retained.extend(active_id.iter().cloned());
    filesystem::clean_staging(self)?;
    filesystem::prune_managers(self, &mut state)?;
    if desktop {
      filesystem::prune_desktops(self, &state)?;
    }
    retained.extend(state.development.values().cloned());
    for entry in &state.entries {
      if entry.current {
        retained.insert(entry.bundle_id.clone());
      }
    }
    let mut result = PruneResult::default();
    if desktop
      && let Some(selection) = &state.desktop
      && !filesystem::desktop_available(self, &selection.content_id)?
    {
      if forget_missing {
        result
          .forgotten_missing_builds
          .push(selection.content_id.clone());
        state.desktop = None;
      } else {
        result.missing_builds.push(selection.content_id.clone());
      }
    }
    if forget_missing {
      let missing = |id: &String| missing_builds.binary_search(id).is_ok();
      if active_id.as_ref().is_some_and(missing) {
        state.active_runtime = None;
      }
      state.entries.retain(|entry| !missing(&entry.bundle_id));
      state.development.retain(|_, id| !missing(id));
      result.forgotten_missing_builds.extend(missing_builds);
      result.forgotten_missing_builds.sort();
      result.forgotten_missing_builds.dedup();
    } else {
      result.missing_builds.extend(missing_builds);
      result.missing_builds.sort();
      result.missing_builds.dedup();
    }
    let mut removable = Vec::new();
    for (id, path) in filesystem::entries(self, &state)? {
      if retained.contains(&id) {
        continue;
      }
      if leases::in_use(self, &id)? {
        result.retained_in_use.push(id);
      } else {
        // A lease publisher holds this same installation lock. Once checked,
        // no process can publish a new lease until deletion is complete.
        removable.push((id, path));
      }
    }
    state
      .entries
      .retain(|entry| !removable.iter().any(|(id, _)| *id == entry.bundle_id));
    if (!removable.is_empty() || !result.forgotten_missing_builds.is_empty())
      && catalog::exists(self)?
    {
      // Remove metadata first: a crash may leave an unregistered payload, but
      // never a catalog entry pointing at bytes already deleted by GC.
      catalog::commit(self, &state)?;
    }
    for (id, path) in removable {
      filesystem::remove_installation(&path)?;
      result.removed.push(id);
    }
    self.release_runtime_paths(&mut state)?;
    Ok(result)
  }

  fn release_runtime_paths(&self, state: &mut catalog::State) -> io::Result<()> {
    let registered: BTreeSet<_> = state
      .entries
      .iter()
      .map(|entry| entry.bundle_id.as_str())
      .collect();
    let mut unused = BTreeSet::new();
    for (id, _) in state
      .runtime_paths
      .locations()
      .filter(|(id, _)| !registered.contains(id.as_str()))
    {
      if filesystem::payload_dir(self, state, id)?.is_none() {
        unused.insert(id.clone());
      }
    }
    let root = self.runtime_root();
    let keep = |id: &str| !unused.contains(id);
    if filesystem::release_unused_locations(&root, &mut state.runtime_paths.target_first, keep)? {
      catalog::commit_runtime_paths(self, &state.runtime_paths.target_first)?;
    }
    if filesystem::release_unused_locations(&root, &mut state.runtime_paths.build_first, keep)? {
      catalog::commit_runtime_build_paths(self, &state.runtime_paths.build_first)?;
    }
    Ok(())
  }

  /// Retains an installation while a client, agent or daemon uses its files.
  ///
  /// # Errors
  /// Rejects payloads outside this account's immutable installation directory.
  pub fn lease(&self, bundle: &Bundle) -> io::Result<Lease> {
    leases::acquire(self, bundle)
  }
}

#[cfg(target_os = "macos")]
fn helper_selection_changed() -> io::Error {
  io::Error::new(
    io::ErrorKind::WouldBlock,
    "the active installation changed during ctld signing; retry renewal",
  )
}

#[cfg(target_os = "macos")]
fn validate_helper_renewal(previous: &Manifest, renewed: &Manifest) -> io::Result<()> {
  const PACKAGE_FILES: [&str; 5] = [
    "ctld-package.json",
    "ctld.app/Contents/Info.plist",
    "ctld.app/Contents/MacOS/ctld",
    "ctld.app/Contents/_CodeSignature/CodeResources",
    "ctld.app/Contents/embedded.provisionprofile",
  ];
  let signed_local = |manifest: &Manifest| {
    manifest.source == crate::bundles::Source::Local
      && PACKAGE_FILES
        .iter()
        .all(|name| manifest.files.contains_key(*name))
  };
  if !signed_local(previous) || !signed_local(renewed) {
    return Err(filesystem::invalid(
      "signing renewal requires a complete local ctld app package and receipt",
    ));
  }
  let helper_file = |name: &str| {
    name == "ctld" || name == "ctld-package.json" || name.starts_with("ctld.app/Contents/")
  };
  if renewed.target_triple != previous.target_triple
    || renewed.distribution_id != previous.distribution_id
    || renewed.components != previous.components
    || previous
      .files
      .iter()
      .filter(|(name, _)| !helper_file(name))
      .ne(renewed.files.iter().filter(|(name, _)| !helper_file(name)))
  {
    return Err(filesystem::invalid(
      "signing renewal changed files or metadata outside the ctld helper package",
    ));
  }
  Ok(())
}

fn catalog_entry<'a>(state: &'a catalog::State, id: &str) -> Option<&'a CatalogEntry> {
  state.entries.iter().find(|entry| entry.bundle_id == id)
}

fn verify_channel_heads(
  state: &catalog::State,
  channel: &str,
  expected: &[CatalogEntry],
) -> io::Result<()> {
  let mut targets = BTreeSet::new();
  for entry in expected {
    entry.validate()?;
    if !entry.current
      || entry
        .origin
        .as_ref()
        .is_none_or(|origin| origin.channel != channel)
      || !targets.insert(&entry.target_triple)
    {
      return Err(filesystem::invalid("invalid expected channel heads"));
    }
  }
  let mut expected = expected.iter().collect::<Vec<_>>();
  let mut current = state
    .entries
    .iter()
    .filter(|entry| {
      entry.current
        && entry
          .origin
          .as_ref()
          .is_some_and(|origin| origin.channel == channel)
    })
    .collect::<Vec<_>>();
  expected.sort_by_key(|entry| &entry.target_triple);
  current.sort_by_key(|entry| &entry.target_triple);
  if current != expected {
    return Err(io::Error::new(
      io::ErrorKind::WouldBlock,
      "channel components changed during preparation; refresh the catalog and retry",
    ));
  }
  Ok(())
}
