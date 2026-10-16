//! One selection contract for passive inspection, preparation, and setup.
//!
//! Selecting an executable never selects or replaces a running socket owner.

use crate::{ConnectError, ProtocolVersion};
use ctl_core::component::ComponentInfo;
use serde::Serialize;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
  /// Inspect the selected build without installing or selecting anything.
  Inspect,
  /// Prepare the selected executable for a caller that may launch it.
  Prepare,
  /// Explicitly make the selected build available, preserving running owners.
  Setup,
}

#[derive(Debug, Clone, Copy)]
pub struct Request {
  pub intent: Intent,
  pub required: Option<ProtocolVersion>,
}

impl Request {
  #[must_use]
  pub const fn new(intent: Intent) -> Self {
    Self {
      intent,
      required: None,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
  Override,
  Checkout,
  EmbeddedDevelopment,
  SharedBundle,
  SharedInstallation,
  EmbeddedRelease,
  DesktopBundle,
  Sibling,
  Path,
  OfficialRelease,
}

impl Source {
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Override => "override",
      Self::Checkout => "checkout",
      Self::EmbeddedDevelopment => "embedded_development",
      Self::SharedBundle => "shared_bundle",
      Self::SharedInstallation => "shared_installation",
      Self::EmbeddedRelease => "embedded_release",
      Self::DesktopBundle => "desktop_bundle",
      Self::Sibling => "sibling",
      Self::Path => "path",
      Self::OfficialRelease => "official_release",
    }
  }
}

impl std::fmt::Display for Source {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

/// The desired installation, independent of the daemon currently running.
#[derive(Debug, Clone, Serialize)]
pub struct Selection {
  pub executable: PathBuf,
  pub source: Source,
  pub info: Option<ComponentInfo>,
  pub archive_sha256: Option<String>,
  pub installed: bool,
  /// Whether the executable was already installed when resolution began.
  pub reused: bool,
}

impl Selection {
  /// Returns a launchable path; an inspected future installation is not runnable.
  ///
  /// # Errors
  /// Reports a selected helper that still needs explicit setup or preparation.
  pub fn ready_executable(self) -> io::Result<PathBuf> {
    if !self.installed {
      let guidance = if matches!(self.source, Source::Path | Source::Sibling) {
        "Build/install ctld beside ctl or on PATH, or set CTLD_BIN."
      } else {
        "Run ctl setup, then retry."
      };
      return Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("The selected ctld helper is not installed. {guidance}"),
      ));
    }
    Ok(self.executable)
  }
}

pub type ResolutionFuture = Pin<Box<dyn Future<Output = io::Result<Option<Selection>>> + Send>>;
pub type Resolver = fn(Request) -> ResolutionFuture;
static RESOLVER: OnceLock<Resolver> = OnceLock::new();

/// Registers the client's shared policy without inspecting or installing code.
///
/// # Errors
/// Rejects multiple registrations in the same process.
pub fn register(resolver: Resolver) -> io::Result<()> {
  RESOLVER.set(resolver).map_err(|_| {
    io::Error::new(
      io::ErrorKind::AlreadyExists,
      "a ctld resolver is already registered",
    )
  })
}

pub(crate) fn registered() -> bool {
  RESOLVER.get().is_some()
}

/// Resolves the same selected build for status, setup, startup, and restart.
/// Explicit overrides are authoritative for every intent, including setup.
/// Inspect never invokes a legacy preparation callback.
///
/// # Errors
/// Reports invalid selections, verification failures, or unavailable helpers.
pub async fn resolve(request: Request) -> Result<Selection, ConnectError> {
  if let Some(executable) = std::env::var_os(crate::DAEMON_EXECUTABLE_ENV) {
    return inspect_executable(Path::new(&executable), Source::Override, request)
      .await
      .map_err(ConnectError::PrepareDaemon);
  }
  if let Some(resolver) = RESOLVER.get() {
    return resolver(request)
      .await
      .map_err(ConnectError::PrepareDaemon)?
      .ok_or_else(|| {
        ConnectError::PrepareDaemon(io::Error::new(
          io::ErrorKind::NotFound,
          "No ctld helper is selected. Run ctl setup, then retry.",
        ))
      });
  }
  // Compatibility for library consumers still registering the earlier providers.
  // New clients register one resolver and never take this path.
  let executable = match request.intent {
    Intent::Inspect => crate::daemon_executable()?,
    Intent::Prepare | Intent::Setup => {
      crate::legacy_prepare_daemon_with_contract(request.required).await?
    }
  };
  inspect_executable(&executable, Source::Path, request)
    .await
    .map_err(ConnectError::PrepareDaemon)
}

/// Inspects an explicitly selected loose executable without installing it.
///
/// # Errors
/// Rejects missing, changing, malformed, or incompatible executable metadata.
pub async fn inspect_executable(
  executable: &Path,
  source: Source,
  request: Request,
) -> io::Result<Selection> {
  let prepared = ctl_core::executable::PreparedExecutable::prepare(
    executable.to_owned(),
    &[
      ("ctld", crate::SUPPORTED_PROTOCOL_VERSIONS),
      (
        "ctld_lifecycle",
        crate::lifecycle::SUPPORTED_PROTOCOL_VERSIONS,
      ),
    ],
  )
  .await?;
  if request.required.is_some_and(|required| {
    !prepared
      .info
      .protocols
      .iter()
      .any(|entry| entry.name == "ctld_helper" && entry.supports(required))
  }) {
    return Err(io::Error::other(
      "The selected ctld does not advertise the required helper contract",
    ));
  }
  prepared.verify().await?;
  Ok(Selection {
    executable: prepared.path,
    source,
    info: Some(prepared.info),
    archive_sha256: None,
    installed: true,
    reused: true,
  })
}

/// Finds an app-contained helper for a client inside a desktop bundle.
#[must_use]
pub fn desktop_candidate() -> Option<PathBuf> {
  let executable = std::env::current_exe().ok()?;
  let contents = executable.parent()?.parent()?;
  let candidate = contents.join("Helpers/ctld.app/Contents/MacOS/ctld");
  candidate.is_file().then_some(candidate)
}

/// Finds an existing explicit complete bundle, sibling, or PATH helper.
/// Missing loose candidates are represented by `None`, never executed.
///
/// # Errors
/// Reports invalid complete selections or invalid candidate metadata.
pub async fn fallback(request: Request) -> io::Result<Option<Selection>> {
  #[cfg(unix)]
  if let Some(executable) =
    crate::selected_bundle_daemon(dirs::home_dir().as_deref()).map_err(io::Error::other)?
  {
    return inspect_executable(&executable, Source::SharedBundle, request)
      .await
      .map(Some);
  }
  let current = std::env::current_exe()?;
  let sibling = current.with_file_name(format!("ctld{}", std::env::consts::EXE_SUFFIX));
  let (candidate, source) = if sibling.is_file() {
    (sibling, Source::Sibling)
  } else {
    let bare = PathBuf::from(format!("ctld{}", std::env::consts::EXE_SUFFIX));
    match crate::resolve_executable(&bare) {
      Ok(path) => (path, Source::Path),
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
      Err(error) => return Err(error),
    }
  };
  inspect_executable(&candidate, source, request)
    .await
    .map(Some)
}

#[cfg(all(test, unix))]
mod tests;
