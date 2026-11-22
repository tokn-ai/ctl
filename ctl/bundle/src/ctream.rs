//! Shared ctream bundle names, viewer targets, and agent layout.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(feature = "build-metadata")]
mod build;
#[cfg(feature = "build-metadata")]
pub use build::BuildMetadata;

#[cfg(all(feature = "install", any(unix, windows)))]
mod install;
#[cfg(all(feature = "install", any(unix, windows)))]
pub use install::{Installation, active, install, install_development, root_for_account};

/// Directory containing the bundled agents beside a ctream viewer.
pub const AGENT_DIRECTORY: &str = "ctream-agents";
/// Largest individual agent or helper supported by the bundle and sync path.
pub const MAX_AGENT_BYTES: u64 = 128 * 1024 * 1024;
/// Largest compressed workflow bundle accepted by the installer.
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
/// Largest total uncompressed payload accepted by the installer.
pub const MAX_PAYLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// Canonical agent directory in a bundle, regardless of the build toolchain.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AgentTarget {
  /// Linux x86-64.
  LinuxX64,
  /// Linux ARM64.
  LinuxArm64,
  /// Native Windows x86-64.
  WindowsX64,
}

impl AgentTarget {
  /// All agent targets in a complete workflow bundle.
  pub const ALL: [Self; 3] = [Self::LinuxX64, Self::LinuxArm64, Self::WindowsX64];

  /// Canonical target triple used as the agent directory name.
  #[must_use]
  pub const fn triple(self) -> &'static str {
    match self {
      Self::LinuxX64 => "x86_64-unknown-linux-musl",
      Self::LinuxArm64 => "aarch64-unknown-linux-musl",
      Self::WindowsX64 => "x86_64-pc-windows-msvc",
    }
  }

  /// Agent executable name for this target.
  #[must_use]
  pub const fn agent(self) -> &'static str {
    if self.is_windows() {
      "ctream-agent.exe"
    } else {
      "ctream-agent"
    }
  }

  /// Auxiliary executables shipped with this agent in workflow bundles.
  #[must_use]
  pub const fn companions(self) -> &'static [&'static str] {
    if self.is_windows() {
      &[]
    } else {
      &["ctream-wayland-input", "ctream-wayland-window"]
    }
  }

  /// Whether the agent runs on Windows.
  #[must_use]
  pub const fn is_windows(self) -> bool {
    matches!(self, Self::WindowsX64)
  }

  /// Find the agent directory beneath a viewer's containing directory.
  #[must_use]
  pub fn directory(self, base: &Path) -> PathBuf {
    base.join(AGENT_DIRECTORY).join(self.triple())
  }

  /// Slash-delimited package path for a named executable in this target.
  #[must_use]
  pub fn relative_path(self, name: &str) -> String {
    format!("{}/{name}", self.relative_directory())
  }

  fn relative_directory(self) -> String {
    format!("{AGENT_DIRECTORY}/{}", self.triple())
  }

  /// Agent target for the current OS and architecture, if supported.
  #[must_use]
  pub const fn native() -> Option<Self> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
      Some(Self::LinuxX64)
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
      Some(Self::LinuxArm64)
    } else if cfg!(all(windows, target_arch = "x86_64")) {
      Some(Self::WindowsX64)
    } else {
      None
    }
  }

  /// Parse a canonical bundle target triple.
  #[must_use]
  pub fn from_triple(triple: &str) -> Option<Self> {
    Self::ALL
      .into_iter()
      .find(|target| target.triple() == triple)
  }
}

/// Relative archive paths of all agents and companions in a workflow bundle.
pub fn agent_paths() -> impl Iterator<Item = String> {
  AgentTarget::ALL.into_iter().flat_map(|target| {
    std::iter::once(target.agent())
      .chain(target.companions().iter().copied())
      .map(move |name| target.relative_path(name))
  })
}

/// Artifact name and archive's root directory for a viewer target.
#[must_use]
pub fn archive_name(viewer_target: &str) -> String {
  format!("ctream-preview-{viewer_target}")
}

/// Preview and CLI filenames, in that order, for a viewer target.
#[must_use]
pub fn viewer_executables(viewer_target: &str) -> [&'static str; 2] {
  if viewer_target.contains("-pc-windows-") {
    ["ctream-preview.exe", "ctream.exe"]
  } else {
    ["ctream-preview", "ctream"]
  }
}

/// Whether the ctream workflow publishes a viewer bundle for this target.
#[must_use]
pub fn has_workflow_bundle(viewer_target: &str) -> bool {
  matches!(
    viewer_target,
    "aarch64-apple-darwin" | "x86_64-unknown-linux-gnu" | "x86_64-pc-windows-msvc"
  )
}

/// Return the viewer target for this platform.
///
/// # Errors
/// Returns an unsupported-platform error when no native viewer target is known.
pub fn native_target() -> io::Result<&'static str> {
  match (std::env::consts::OS, std::env::consts::ARCH) {
    ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
    ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
    // ctlup itself may use musl. The GUI preview uses the system Linux ABI.
    ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
    ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
    ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
    ("windows", "aarch64") => Ok("aarch64-pc-windows-msvc"),
    _ => Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "ctream does not support this viewer platform",
    )),
  }
}
