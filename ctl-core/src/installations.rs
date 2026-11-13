//! One active complete installation with stable commands and bounded retention.
//!
//! Installing changes only the next executable launch. Existing processes hold
//! leases on immutable payloads and continue using their original installation.

mod catalog;
mod filesystem;
mod leases;
mod migration;
#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, windows))]
mod windows_tests;

use crate::bundles::{Bundle, Manifest};
#[cfg(unix)]
use crate::bundles::{Purpose, Store};
use serde::Serialize;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

pub use catalog::{CatalogEntry, CatalogOrigin};
pub use leases::{Lease, lease_current_executable};

/// Complete installations for one account, with one native execution selection.
/// Catalogued foreign targets are retained for deployment without execution.
#[derive(Debug, Clone)]
pub struct Installations {
  home: PathBuf,
  root: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PruneResult {
  pub removed: Vec<String>,
  pub retained_in_use: Vec<String>,
}

impl Installations {
  /// Merely constructs paths; reading status never prepares an installation.
  #[must_use]
  pub fn new(home: &Path) -> Self {
    Self {
      home: home.to_owned(),
      root: home.join(".tokn/ctl"),
    }
  }

  #[must_use]
  pub fn root(&self) -> &Path {
    &self.root
  }

  /// Reads the active payload, including a verified pre-ctlup installation.
  ///
  /// # Errors
  /// A broken pointer, unsafe directory or modified payload is never hidden by
  /// selecting a different installation.
  pub fn active(&self) -> io::Result<Option<Bundle>> {
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    let migrated = migration::is_migrated(self)?;
    if let Some(bundle) = filesystem::pointer(self, "current", true)? {
      return Ok(Some(bundle));
    }
    #[cfg(unix)]
    {
      if migrated {
        return Ok(None);
      }
      if filesystem::path_exists(&self.root.join("current"))? {
        // A historical agent-only overlay is not a complete installation. Keep
        // it operational until an explicit complete install supersedes it.
        return Ok(None);
      }
      Store::new(&self.home).selected(Purpose::Local, crate::paths::native_target())
    }
    #[cfg(windows)]
    {
      let _ = migrated;
      Ok(None)
    }
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
    let _lock = filesystem::Lock::acquire(self)?;
    let Some(bundle) = self.active()? else {
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

  /// Reads the single rollback candidate without changing any pointer.
  ///
  /// # Errors
  /// Reports an unsafe, damaged or incompatible rollback payload.
  pub fn previous(&self) -> io::Result<Option<Bundle>> {
    if !filesystem::root_exists(self)? {
      return Ok(None);
    }
    filesystem::pointer(self, "previous", false)
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
    filesystem::load(self, bundle_id)
  }

  /// Lists verified installations without changing the active build.
  ///
  /// # Errors
  /// Reports unsafe or corrupt entries rather than hiding them from inspection.
  pub fn list(&self) -> io::Result<Vec<Bundle>> {
    if !filesystem::root_exists(self)? {
      return Ok(Vec::new());
    }
    filesystem::entries(self)?
      .into_iter()
      .map(|(id, _)| filesystem::load(self, &id))
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
    filesystem::ensure_root(self)?;
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::recover(self)?;
    let manifests = self
      .list()?
      .into_iter()
      .map(|bundle| bundle.manifest)
      .collect::<Vec<_>>();
    operation(&manifests)
  }

  /// Lists installed payloads and their recorded channel provenance.
  /// Historical installations without provenance remain visible as unknown.
  ///
  /// # Errors
  /// Reports unsafe storage, damaged catalog entries or modified payloads.
  pub fn catalog(&self) -> io::Result<Vec<CatalogEntry>> {
    if !filesystem::root_exists(self)? {
      return Ok(Vec::new());
    }
    catalog::inventory(self)
  }

  /// Explicitly adopts existing complete installations into a durable catalog.
  /// Existing provenance, native history and live leases are preserved. Local
  /// source builds are adopted as local; other uncatalogued builds as provided.
  /// No historical build date or original acquisition channel is inferred.
  ///
  /// # Errors
  /// Reports unsafe storage, incomplete old builds or failed durable migration.
  pub fn migrate_catalog(&self) -> io::Result<Vec<CatalogEntry>> {
    migration::migrate(self)
  }

  /// Checks the explicit catalog migration marker without changing storage.
  ///
  /// # Errors
  /// Reports malformed markers or a marker whose catalog is missing.
  pub fn is_catalog_migrated(&self) -> io::Result<bool> {
    migration::is_migrated(self)
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
    catalog::recover(self)?;
    let mut state = catalog::read(self)?;
    if let Some(expected_heads) = expected_heads {
      verify_channel_heads(&state, &origin.channel, expected_heads)?;
    }
    let installed = bundles
      .iter()
      .map(|bundle| filesystem::publish(self, bundle, None))
      .collect::<io::Result<Vec<_>>>()?;
    let previous = native
      .map(|_| {
        self
          .active()?
          .map(|bundle| filesystem::publish(self, &bundle, None))
          .transpose()
      })
      .transpose()?
      .flatten();
    state.register(&installed, origin, false);
    let activation = native.map(|index| (&installed[index], previous.as_ref()));
    catalog::commit(self, &state, activation)?;
    Ok(installed)
  }

  /// Unregisters a channel target. Active, rollback and leased payloads survive
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
    if !removed {
      return Ok(false);
    }
    catalog::commit(self, &state, None)?;
    Ok(true)
  }

  /// Verifies, publishes and atomically activates one complete native build.
  /// The old complete build becomes the rollback candidate. Running services
  /// are neither inspected nor restarted.
  ///
  /// # Errors
  /// Rejects incomplete or foreign builds, unsafe storage, changed bytes and
  /// command collisions. A pending transaction is recovered before a new one.
  pub fn activate(&self, bundle: &Bundle) -> io::Result<Bundle> {
    self
      .activate_with(bundle, None, None, |_| Ok(()))
      .map(|(installed, ())| installed)
  }

  /// Activates and retains a build in one transaction, so another updater
  /// cannot prune it before its launcher obtains a process lease.
  ///
  /// # Errors
  /// Reports activation errors or an unavailable lease before switching builds.
  pub fn activate_and_lease(&self, bundle: &Bundle) -> io::Result<(Bundle, Lease)> {
    self.activate_with(bundle, None, None, |installed| {
      leases::acquire_locked(self, installed)
    })
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
      .activate_with(bundle, Some(origin), None, |_| Ok(()))
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
    self.activate_with(bundle, Some(origin), None, |installed| {
      leases::acquire_locked(self, installed)
    })
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
    self.activate_with(bundle, Some(origin), Some(expected_heads), |installed| {
      leases::acquire_locked(self, installed)
    })
  }

  fn activate_with<T>(
    &self,
    bundle: &Bundle,
    origin: Option<&CatalogOrigin>,
    expected_heads: Option<&[CatalogEntry]>,
    prepare: impl FnOnce(&Bundle) -> io::Result<T>,
  ) -> io::Result<(Bundle, T)> {
    filesystem::validate_native(bundle, false)?;
    if let Some(origin) = origin {
      origin.validate()?;
    }
    let files = bundle.read_files()?;
    filesystem::ensure_root(self)?;
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::recover(self)?;
    if origin.is_none()
      && migration::is_migrated(self)?
      && !catalog::read(self)?
        .entries
        .iter()
        .any(|entry| entry.bundle_id == bundle.manifest.bundle_id)
    {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "new installations require an explicit catalog origin after migration",
      ));
    }
    if let (Some(origin), Some(expected_heads)) = (origin, expected_heads) {
      verify_channel_heads(&catalog::read(self)?, &origin.channel, expected_heads)?;
    }
    let previous = self
      .active()?
      .map(|bundle| filesystem::publish(self, &bundle, None))
      .transpose()?;
    let installed = filesystem::publish(self, bundle, Some(&files))?;
    let retained = prepare(&installed)?;
    if let Some(origin) = origin {
      let mut state = catalog::read(self)?;
      state.register(std::slice::from_ref(&installed), origin, true);
      catalog::commit(self, &state, Some((&installed, previous.as_ref())))?;
    } else {
      filesystem::activate(self, &installed, previous.as_ref())?;
    }
    Ok((installed, retained))
  }

  /// Switches the complete active and previous payloads, without restarting.
  ///
  /// # Errors
  /// Reports absent or damaged rollback content and unsafe storage.
  pub fn rollback(&self) -> io::Result<Bundle> {
    filesystem::ensure_root(self)?;
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::recover(self)?;
    let previous = self.previous()?.ok_or_else(|| {
      io::Error::new(io::ErrorKind::NotFound, "there is no previous installation")
    })?;
    let active = self
      .active()?
      .map(|bundle| filesystem::publish(self, &bundle, None))
      .transpose()?;
    filesystem::activate(self, &previous, active.as_ref())?;
    Ok(previous)
  }

  /// Keeps registered channel targets, active and previous payloads, and builds
  /// leased by live processes.
  ///
  /// # Errors
  /// Unsafe entries or unreadable leases stop pruning without guessing whether
  /// a process is alive. No PID or time-based ownership heuristic is used.
  pub fn prune(&self) -> io::Result<PruneResult> {
    if !filesystem::root_exists(self)? {
      return Ok(PruneResult::default());
    }
    let _lock = filesystem::Lock::acquire(self)?;
    catalog::recover(self)?;
    self.prune_locked()
  }

  fn prune_locked(&self) -> io::Result<PruneResult> {
    filesystem::clean_staging(self)?;
    #[cfg(windows)]
    filesystem::prune_managers(self)?;
    let mut retained = BTreeSet::new();
    for bundle in [self.active()?, self.previous()?].into_iter().flatten() {
      retained.insert(bundle.manifest.bundle_id);
    }
    let mut state = catalog::read(self)?;
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
