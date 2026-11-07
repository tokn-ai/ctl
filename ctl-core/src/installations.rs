//! One active complete installation with stable commands and bounded retention.
//!
//! Installing changes only the next executable launch. Existing processes hold
//! leases on immutable payloads and continue using their original installation.

mod filesystem;
mod leases;
#[cfg(test)]
mod tests;

use crate::bundles::{Bundle, Purpose, Store};
use serde::Serialize;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

pub use leases::{Lease, lease_current_executable};

/// Complete installations for one account. Foreign target archives are cache
/// entries, never another selected installation on this account.
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
    if let Some(bundle) = filesystem::pointer(self, "current", true)? {
      return Ok(Some(bundle));
    }
    if filesystem::path_exists(&self.root.join("current"))? {
      // A historical agent-only overlay is not a complete installation. Keep
      // it operational until an explicit complete install supersedes it.
      return Ok(None);
    }
    Store::new(&self.home).selected(Purpose::Local, crate::paths::native_target())
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

  /// Opens one immutable native installation by its content identity.
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

  /// Lists verified native installations without changing the active build.
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

  /// Verifies, publishes and atomically activates one complete native build.
  /// The old complete build becomes the rollback candidate. Running services
  /// are neither inspected nor restarted.
  ///
  /// # Errors
  /// Rejects incomplete or foreign builds, unsafe storage, changed bytes and
  /// command collisions. A pending transaction is recovered before a new one.
  pub fn activate(&self, bundle: &Bundle) -> io::Result<Bundle> {
    filesystem::validate_native(bundle, false)?;
    let files = bundle.read_files()?;
    filesystem::ensure_root(self)?;
    let _lock = filesystem::Lock::acquire(self)?;
    filesystem::recover(self)?;
    let previous = self
      .active()?
      .map(|bundle| filesystem::publish(self, &bundle, None))
      .transpose()?;
    let installed = filesystem::publish(self, bundle, Some(&files))?;
    filesystem::activate(self, &installed, previous.as_ref())?;
    Ok(installed)
  }

  /// Switches the complete active and previous payloads, without restarting.
  ///
  /// # Errors
  /// Reports absent or damaged rollback content and unsafe storage.
  pub fn rollback(&self) -> io::Result<Bundle> {
    filesystem::ensure_root(self)?;
    let _lock = filesystem::Lock::acquire(self)?;
    filesystem::recover(self)?;
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

  /// Keeps only active, previous, and payloads leased by live processes.
  ///
  /// # Errors
  /// Unsafe entries or unreadable leases stop pruning without guessing whether
  /// a process is alive. No PID or time-based ownership heuristic is used.
  pub fn prune(&self) -> io::Result<PruneResult> {
    if !filesystem::root_exists(self)? {
      return Ok(PruneResult::default());
    }
    let _lock = filesystem::Lock::acquire(self)?;
    filesystem::recover(self)?;
    self.prune_locked()
  }

  fn prune_locked(&self) -> io::Result<PruneResult> {
    filesystem::clean_staging(self)?;
    let mut retained = BTreeSet::new();
    for bundle in [self.active()?, self.previous()?].into_iter().flatten() {
      retained.insert(bundle.manifest.bundle_id);
    }
    let mut result = PruneResult::default();
    for (id, path) in filesystem::entries(self)? {
      if retained.contains(&id) {
        continue;
      }
      if leases::in_use(self, &id)? {
        result.retained_in_use.push(id);
      } else {
        // A lease publisher holds this same installation lock. Once checked,
        // no process can publish a new lease until deletion is complete.
        filesystem::remove_installation(&path)?;
        result.removed.push(id);
      }
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
