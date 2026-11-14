//! One active complete installation with stable commands and bounded retention.
//!
//! Installing changes only the next executable launch. Existing processes hold
//! leases on immutable payloads and continue using their original installation.

mod catalog;
mod filesystem;
mod layout;
mod leases;
mod migration;
#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, windows))]
mod windows_tests;

use crate::bundles::{Bundle, Manifest};
use serde::Serialize;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

pub use catalog::{CatalogEntry, CatalogOrigin};
pub use leases::{Lease, lease_current_executable};

/// Local updater capability for preserving development checkout registrations.
pub const DEVELOPMENT_LAYOUT_VERSION: u16 = 1;

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

  /// Reads the recorded complete active payload through its canonical selection.
  ///
  /// # Errors
  /// A broken pointer, unsafe directory or modified payload is never hidden by
  /// selecting a different installation.
  pub fn active(&self) -> io::Result<Option<Bundle>> {
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    self.migrate_once_if_needed()?;
    self.recorded_selection("current")
  }

  /// Resolves the active installation and keeps it available for a new process.
  ///
  /// # Errors
  /// Reports damaged installations or unavailable storage or leases.
  #[cfg(windows)]
  pub fn active_and_lease(&self) -> io::Result<Option<(Bundle, Lease)>> {
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    self.migrate_once_if_needed()?;
    let _lock = filesystem::Lock::acquire(self)?;
    let Some(bundle) = self.recorded_selection("current")? else {
      return Ok(None);
    };
    let lease = leases::retain(&self.root.join("leases"), &bundle.manifest.bundle_id)?;
    Ok(Some((bundle, lease)))
  }

  /// Publishes a verified Windows updater and selects it for future invocations.
  /// Existing invocations continue using their leased updater payloads.
  ///
  /// # Errors
  /// Reports unsafe storage, damaged aliases or collisions with other commands.
  #[cfg(windows)]
  pub fn install_manager(&self, updater: &[u8]) -> io::Result<()> {
    filesystem::install_manager(self, updater)
  }

  /// Resolves and pins the active Windows updater for a new invocation.
  ///
  /// # Errors
  /// Reports unsafe storage, damaged updater content or unavailable leases.
  #[cfg(windows)]
  pub fn manager_and_lease(&self) -> io::Result<Option<(PathBuf, Lease)>> {
    filesystem::manager_and_lease(self)
  }

  /// Reads a legacy previous selection without changing its pointer.
  ///
  /// # Errors
  /// Reports an unsafe, damaged or incompatible previous payload.
  pub fn previous(&self) -> io::Result<Option<Bundle>> {
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    self.migrate_once_if_needed()?;
    self.recorded_selection("previous")
  }

  fn recorded_selection(&self, name: &str) -> io::Result<Option<Bundle>> {
    let state = catalog::read_index(self)?;
    let selected = filesystem::pointer(self, name)?;
    if selected.as_ref().is_some_and(|bundle| {
      !state.entries.iter().any(|entry| {
        entry.bundle_id == bundle.manifest.bundle_id
          && entry.target_triple == bundle.manifest.target_triple
      })
    }) {
      return Err(filesystem::invalid(
        "installation selection has no record in the catalog",
      ));
    }
    Ok(selected)
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
    self.migrate_once_if_needed()?;
    self.get_recorded(bundle_id)
  }

  fn get_recorded(&self, bundle_id: &str) -> io::Result<Bundle> {
    let state = catalog::read_index(self)?;
    let Some(entry) = state
      .entries
      .iter()
      .find(|entry| entry.bundle_id == bundle_id)
    else {
      return Err(io::Error::new(
        io::ErrorKind::NotFound,
        "installation is not recorded in the catalog",
      ));
    };
    let bundle = filesystem::load(self, bundle_id)?;
    if bundle.manifest.target_triple != entry.target_triple {
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
    self.migrate_once_if_needed()?;
    self.list_recorded()
  }

  fn list_recorded(&self) -> io::Result<Vec<Bundle>> {
    catalog::read_index(self)?
      .entries
      .into_iter()
      .map(|entry| (entry.bundle_id, entry.target_triple))
      .collect::<std::collections::BTreeMap<_, _>>()
      .into_iter()
      .map(|(id, target)| {
        let bundle = filesystem::load(self, &id)?;
        if bundle.manifest.target_triple != target {
          return Err(filesystem::invalid(
            "catalog target differs from its immutable payload",
          ));
        }
        Ok(bundle)
      })
      .collect()
  }

  /// Runs filesystem maintenance against verified installation manifests while
  /// holding the installation lock. This also prepares an absent store.
  ///
  /// The callback must not mutate the installation store or acquire its lock
  /// again. Cache locks must be acquired inside the callback, after this lock.
  ///
  /// # Errors
  /// Reports unsafe storage, recovery or payload verification failures, and
  /// forwards any error returned by the callback.
  pub fn with_locked_manifests<T>(
    &self,
    operation: impl FnOnce(&[Manifest]) -> io::Result<T>,
  ) -> io::Result<T> {
    self.migrate_once_if_needed()?;
    filesystem::ensure_root(self)?;
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::recover(self)?;
    let manifests = self
      .list_recorded()?
      .into_iter()
      .map(|bundle| bundle.manifest)
      .collect::<Vec<_>>();
    operation(&manifests)
  }

  /// Lists recorded installations and their channel provenance.
  ///
  /// # Errors
  /// Reports unsafe storage, damaged catalog entries or modified payloads.
  pub fn catalog(&self) -> io::Result<Vec<CatalogEntry>> {
    if !filesystem::root_exists(self)? {
      return Ok(Vec::new());
    }
    self.migrate_once_if_needed()?;
    catalog::inventory(self)
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
    self.migrate_once_if_needed()?;
    let _lock = filesystem::Lock::acquire(self)?;
    Ok(catalog::read(self)?.development.remove(checkout_id))
  }

  /// Reports whether updater replacement must preserve development registrations.
  ///
  /// # Errors
  /// Reports unsafe storage or a damaged catalog.
  pub fn has_development_heads(&self) -> io::Result<bool> {
    if !filesystem::root_exists(self)? {
      return Ok(false);
    }
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::has_development_heads(self)
  }

  /// Migrates an existing store before its first use by this updater.
  /// Fresh stores require no migration.
  ///
  /// # Errors
  /// Reports unsafe storage, incompatible old builds or failed migration.
  pub fn migrate_once_if_needed(&self) -> io::Result<()> {
    migration::migrate_once_if_needed(self)
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
    self.migrate_once_if_needed()?;
    filesystem::ensure_root(self)?;
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::recover(self)?;
    let mut state = catalog::read(self)?;
    if let Some(expected_heads) = expected_heads {
      verify_channel_heads(&state, &origin.channel, expected_heads)?;
    }
    catalog::initialize(self, &state)?;
    if native.is_some() {
      self.recorded_selection("current")?;
    }
    let installed = bundles
      .iter()
      .map(|bundle| filesystem::publish(self, bundle, None))
      .collect::<io::Result<Vec<_>>>()?;
    state.register(&installed, origin, false);
    let activation = native.map(|index| (&installed[index], None));
    catalog::commit(self, &state, activation)?;
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
    self.migrate_once_if_needed()?;
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::recover(self)?;
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
    catalog::commit(self, &state, None)?;
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
      |installed| leases::acquire_locked(self, installed),
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
      |installed| leases::acquire_locked(self, installed),
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
      |installed| leases::acquire_locked(self, installed),
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
    self.activate_with(
      bundle,
      Some(origin),
      None,
      |state| {
        if state.development.get(checkout_id).map(String::as_str) != expected_head {
          return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "development checkout was updated during preparation; retry the build",
          ));
        }
        self.verify_development_siblings(state, bundle, origin)?;
        state
          .development
          .insert(checkout_id.into(), bundle.manifest.bundle_id.clone());
        Ok(())
      },
      |installed| leases::acquire_locked(self, installed),
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
        || &filesystem::load(self, &entry.bundle_id)?
          .manifest
          .components["ctl-agent"]
          .build
          != build
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
    self.migrate_once_if_needed()?;
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::recover(self)?;
    let active = self
      .recorded_selection("current")?
      .filter(|active| active.manifest.bundle_id == expected_active)
      .ok_or_else(helper_selection_changed)?;
    validate_helper_renewal(&active.manifest, &bundle.manifest)?;
    if active.manifest.bundle_id == bundle.manifest.bundle_id {
      let lease = leases::acquire_locked(self, &active)?;
      return Ok((active, lease));
    }

    let mut state = catalog::read(self)?;
    state.replace_helper_memberships(expected_active, &bundle.manifest.bundle_id)?;
    let installed = filesystem::publish(self, bundle, Some(&files))?;
    let lease = leases::acquire_locked(self, &installed)?;
    catalog::commit(self, &state, Some((&installed, None)))?;
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
    self.migrate_once_if_needed()?;
    filesystem::ensure_root(self)?;
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::recover(self)?;
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
    self.recorded_selection("current")?;
    // Initialize using the persisted state, since the checkout update below
    // may refer to a bundle that has yet to be published and recorded.
    catalog::initialize(self, &state)?;
    update(&mut state)?;
    let installed = filesystem::publish(self, bundle, Some(&files))?;
    let retained = prepare(&installed)?;
    if let Some(origin) = origin {
      state.register(std::slice::from_ref(&installed), origin, true);
    } else if let Some(origin) = inferred_origin {
      state.register(std::slice::from_ref(&installed), &origin, true);
    }
    catalog::commit(self, &state, Some((&installed, None)))?;
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
        || filesystem::load(self, &entry.bundle_id)?
          .manifest
          .components["ctl-agent"]
          .build
          != bundle.manifest.components["ctl-agent"].build
      {
        return Err(io::Error::new(
          io::ErrorKind::InvalidInput,
          "inferred provided installation cannot replace other target components; install the complete matching channel cohort",
        ));
      }
    }
    Ok(origin)
  }

  /// Keeps registered targets, the active payload, and builds
  /// leased by live processes.
  ///
  /// # Errors
  /// Unsafe entries or unreadable leases stop pruning without guessing whether
  /// a process is alive. No PID or time-based ownership heuristic is used.
  pub fn prune(&self) -> io::Result<PruneResult> {
    if !filesystem::root_exists(self)? {
      return Ok(PruneResult::default());
    }
    self.migrate_once_if_needed()?;
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::recover(self)?;
    self.prune_locked()
  }

  fn prune_locked(&self) -> io::Result<PruneResult> {
    filesystem::clean_staging(self)?;
    #[cfg(windows)]
    filesystem::prune_managers(self)?;
    let mut retained = BTreeSet::new();
    if let Some(bundle) = self.recorded_selection("current")? {
      retained.insert(bundle.manifest.bundle_id);
    }
    let mut state = catalog::read(self)?;
    filesystem::clear_previous(self)?;
    retained.extend(state.development.values().cloned());
    for entry in &state.entries {
      if entry.current {
        retained.insert(entry.bundle_id.clone());
      }
    }
    let mut result = PruneResult::default();
    let mut removable = Vec::new();
    for (id, path) in filesystem::entries(self)? {
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
    if !removable.is_empty() && catalog::exists(self)? {
      // Remove metadata first: a crash may leave an unregistered payload, but
      // never a catalog entry pointing at bytes already deleted by GC.
      catalog::commit(self, &state, None)?;
    } else if catalog::exists(self)? {
      layout::refresh(self, &state);
    }
    for (id, path) in removable {
      filesystem::remove_installation(&path)?;
      result.removed.push(id);
    }
    Ok(result)
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
