//! Shared ctld selection for CLI and desktop clients.
//!
//! All intents choose a candidate with the same policy. Inspection never
//! publishes it; preparation makes that exact candidate runnable. Socket-owner
//! reuse and explicit activation remain the IPC lifecycle's responsibility.

use crate::setup::{self, DevelopmentContext, HelperSelection};
use ctl_ipc::resolver::{Intent, Request, Selection, Source};
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleMode {
  Signed,
  Development,
}

#[derive(Clone, Copy)]
pub struct EmbeddedBundle {
  pub manifest: &'static [u8],
  pub archive: &'static [u8],
  pub mode: BundleMode,
}

pub struct Resolver<'a> {
  embedded: Option<EmbeddedBundle>,
  development: Option<&'a DevelopmentContext>,
  desktop: bool,
  #[cfg(all(test, target_os = "macos"))]
  home: Option<&'a Path>,
}

enum Candidate {
  Checkout(Selection),
  Embedded(Selection, EmbeddedBundle),
  Existing(Selection),
  #[cfg(target_os = "macos")]
  OfficialRelease(Selection),
}

impl Candidate {
  fn selection(&self) -> &Selection {
    match self {
      Self::Checkout(selection) | Self::Embedded(selection, _) | Self::Existing(selection) => {
        selection
      }
      #[cfg(target_os = "macos")]
      Self::OfficialRelease(selection) => selection,
    }
  }
}

impl<'a> Resolver<'a> {
  #[must_use]
  pub const fn standalone(
    embedded: Option<EmbeddedBundle>,
    development: Option<&'a DevelopmentContext>,
  ) -> Self {
    Self {
      embedded,
      development,
      desktop: false,
      #[cfg(all(test, target_os = "macos"))]
      home: None,
    }
  }

  #[must_use]
  pub const fn desktop(development: Option<&'a DevelopmentContext>) -> Self {
    Self {
      embedded: None,
      development,
      desktop: true,
      #[cfg(all(test, target_os = "macos"))]
      home: None,
    }
  }

  // Native signing tools must retain their real HOME while package state is
  // isolated. This injection is unavailable in production client builds.
  #[cfg(all(test, target_os = "macos"))]
  pub(crate) const fn with_home(mut self, home: &'a Path) -> Self {
    self.home = Some(home);
    self
  }

  /// Resolves or prepares the selected package without starting any service.
  /// Explicit executable overrides are handled by the IPC resolver before this
  /// policy is invoked. Every successful preparation retains the inspected
  /// package's identity; a changed checkout selection is rejected.
  ///
  /// # Errors
  /// Reports unsafe selections, failed verification, or unavailable packages.
  pub async fn resolve(&self, request: Request) -> io::Result<Option<Selection>> {
    loop {
      match self.resolve_once(request).await {
        Err(setup::Error::Busy) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        result => return result.map_err(io::Error::other),
      }
    }
  }

  async fn resolve_once(&self, request: Request) -> Result<Option<Selection>, setup::Error> {
    #[cfg(not(target_os = "macos"))]
    if request.intent == Intent::Setup {
      return Err(setup::Error::UnsupportedPlatform);
    }
    let candidate = self.select(request).await?;
    if request.intent == Intent::Inspect || candidate.selection().installed {
      return Ok(Some(candidate.selection().clone()));
    }
    let before = candidate.selection().clone();
    let executable = match candidate {
      Candidate::Checkout(_) => {
        self
          .prepare_checkout(request.required)
          .await?
          .ok_or_else(|| {
            setup::Error::Verification("checkout selection changed during preparation".into())
          })?
      }
      Candidate::Embedded(_, bundle) => {
        match bundle.mode {
          BundleMode::Development => {
            setup::prepare_bundled_development_ctld(bundle.manifest, bundle.archive, |_| {}).await?
          }
          BundleMode::Signed => {
            setup::prepare_bundled_ctld(bundle.manifest, bundle.archive, |_| {}).await?
          }
        }
        .executable
      }
      #[cfg(target_os = "macos")]
      Candidate::OfficialRelease(_) => {
        if request.intent != Intent::Setup {
          return Err(setup::Error::Verification(
            "The selected official ctld release is not installed. Run ctl setup, then retry."
              .into(),
          ));
        }
        setup::install_signed_ctld(|_| {}).await?.executable
      }
      Candidate::Existing(_) => {
        return Err(
          io::Error::new(
            io::ErrorKind::NotFound,
            "The selected ctld executable is not installed",
          )
          .into(),
        );
      }
    };
    if before.executable.canonicalize()? != executable.canonicalize()? {
      return Err(setup::Error::Verification(
        "ctld selection changed during preparation; no daemon was started".into(),
      ));
    }
    let mut after =
      ctl_ipc::resolver::inspect_executable(&executable, before.source, request).await?;
    if let Some(info) = &before.info
      && after.info.as_ref().is_none_or(|actual| {
        actual.build != info.build
          || !ctl_core::component::protocols_match(&actual.protocols, &info.protocols)
      })
    {
      return Err(setup::Error::Verification(
        "prepared ctld differs from the inspected build".into(),
      ));
    }
    after.archive_sha256 = before.archive_sha256;
    if after.source == Source::OfficialRelease {
      after.archive_sha256 = installed_release_identity(&after.executable).await?;
    }
    after.reused = before.installed;
    Ok(Some(after))
  }

  async fn select(&self, request: Request) -> Result<Candidate, setup::Error> {
    if let Some(bundle) = self
      .embedded
      .filter(|bundle| bundle.mode == BundleMode::Development)
    {
      return embedded_candidate(bundle, request.required).await;
    }
    if self.embedded.is_none()
      && let Some(context) = self.development
      && let Some(selection) = self.inspect_checkout(context, request.required).await?
    {
      return Ok(Candidate::Checkout(selection_from(
        selection,
        Source::Checkout,
      )));
    }
    // An explicit complete-bundle selection is authoritative, including for a
    // packaged desktop. Ordinary compatible installations remain its fallback.
    if self.desktop {
      if let Some(selection) = setup::inspect_selected_bundle_ctld(request.required).await? {
        return Ok(Candidate::Existing(selection_from(
          selection,
          Source::SharedBundle,
        )));
      }
      if let Some(executable) = ctl_ipc::resolver::desktop_candidate() {
        return Ok(Candidate::Existing(
          ctl_ipc::resolver::inspect_executable(&executable, Source::DesktopBundle, request)
            .await?,
        ));
      }
    }
    let shared = setup::inspect_compatible_ctld(request.required)
      .await?
      .map(|selection| {
        let source = shared_source(&selection.executable);
        selection_from(selection, source)
      });
    if let Some(selection) = shared {
      return Ok(Candidate::Existing(selection));
    }
    if let Some(bundle) = self.embedded {
      return embedded_candidate(bundle, request.required).await;
    }
    if !self.desktop
      && let Some(executable) = ctl_ipc::resolver::desktop_candidate()
    {
      return Ok(Candidate::Existing(
        ctl_ipc::resolver::inspect_executable(&executable, Source::DesktopBundle, request).await?,
      ));
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(selection) = ctl_ipc::resolver::fallback(request).await? {
      return Ok(Candidate::Existing(selection));
    }
    #[cfg(target_os = "macos")]
    return missing_candidate();
    #[cfg(not(target_os = "macos"))]
    Ok(missing_candidate())
  }

  async fn inspect_checkout(
    &self,
    context: &DevelopmentContext,
    required: Option<ctl_core::protocol::ProtocolVersion>,
  ) -> Result<Option<HelperSelection>, setup::Error> {
    #[cfg(all(test, target_os = "macos"))]
    if let Some(home) = self.home {
      return context.inspect_in(home, required).await;
    }
    context.inspect(required).await
  }

  async fn prepare_checkout(
    &self,
    required: Option<ctl_core::protocol::ProtocolVersion>,
  ) -> Result<Option<std::path::PathBuf>, setup::Error> {
    let context = self
      .development
      .ok_or_else(|| setup::Error::Verification("checkout selection lost its provenance".into()))?;
    #[cfg(all(test, target_os = "macos"))]
    if let Some(home) = self.home {
      return context.discover_in(home, required).await;
    }
    context.discover(required).await
  }
}

fn selection_from(selection: HelperSelection, source: Source) -> Selection {
  Selection {
    executable: selection.executable,
    source,
    info: selection.info,
    archive_sha256: selection.archive_sha256,
    installed: selection.installed,
    reused: selection.installed,
  }
}

fn shared_source(executable: &Path) -> Source {
  let managed = dirs::home_dir().map(|home| ctl_ipc::managed::component_directory(&home));
  if managed.is_some_and(|root| executable.starts_with(root.canonicalize().unwrap_or(root))) {
    Source::SharedInstallation
  } else {
    Source::SharedBundle
  }
}

async fn embedded_candidate(
  bundle: EmbeddedBundle,
  required: Option<ctl_core::protocol::ProtocolVersion>,
) -> Result<Candidate, setup::Error> {
  let development = bundle.mode == BundleMode::Development;
  let selection = setup::inspect_bundled_ctld(bundle.manifest, development).await?;
  // Unprepared embedded packages have no executable metadata yet. Their
  // compiled-in receipt can still reject unsupported declared contracts.
  let contracts: Contracts = serde_json::from_slice(bundle.manifest)
    .map_err(|error| setup::Error::InvalidRelease(error.to_string()))?;
  for (name, versions) in [
    ("ctld", ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS),
    (
      "ctld_lifecycle",
      ctl_ipc::lifecycle::SUPPORTED_PROTOCOL_VERSIONS,
    ),
    ("ctld_helper", ctl_ipc::SUPPORTED_HELPER_API_VERSIONS),
  ] {
    if !contracts
      .protocols
      .iter()
      .any(|entry| entry.name == name && entry.negotiate(versions).is_some())
    {
      return Err(setup::Error::Verification(
        "embedded ctld declares incompatible protocols".into(),
      ));
    }
  }
  if required.is_some_and(|required| {
    !contracts
      .protocols
      .iter()
      .any(|entry| entry.name == "ctld_helper" && entry.supports(required))
  }) {
    return Err(setup::Error::Verification(
      "embedded ctld does not advertise the required helper contract".into(),
    ));
  }
  Ok(Candidate::Embedded(
    selection_from(
      selection,
      if development {
        Source::EmbeddedDevelopment
      } else {
        Source::EmbeddedRelease
      },
    ),
    bundle,
  ))
}

#[cfg(target_os = "macos")]
fn missing_candidate() -> Result<Candidate, setup::Error> {
  let home = dirs::home_dir().ok_or(setup::Error::HomeDirectory)?;
  let target = match std::env::consts::ARCH {
    "aarch64" => "aarch64-apple-darwin",
    "x86_64" => "x86_64-apple-darwin",
    _ => return Err(setup::Error::UnsupportedPlatform),
  };
  Ok(Candidate::OfficialRelease(Selection {
    executable: ctl_ipc::managed::component_directory(&home)
      .join("versions")
      .join(format!("{}-{target}", env!("CARGO_PKG_VERSION")))
      .join("ctld.app/Contents/MacOS/ctld"),
    source: Source::OfficialRelease,
    info: None,
    archive_sha256: None,
    installed: false,
    reused: false,
  }))
}

#[cfg(not(target_os = "macos"))]
fn missing_candidate() -> Candidate {
  Candidate::Existing(Selection {
    executable: std::path::PathBuf::from(format!("ctld{}", std::env::consts::EXE_SUFFIX)),
    source: Source::Path,
    info: None,
    archive_sha256: None,
    installed: false,
    reused: false,
  })
}

#[derive(serde::Deserialize)]
struct Contracts {
  protocols: Vec<ctl_core::component::ProtocolInfo>,
}

async fn installed_release_identity(executable: &Path) -> Result<Option<String>, setup::Error> {
  use std::io::Read as _;
  let directory = executable
    .ancestors()
    .nth(4)
    .ok_or_else(|| setup::Error::Verification("installed ctld has no package directory".into()))?;
  let mut receipt = Vec::new();
  std::fs::File::open(directory.join("installation.json"))?
    .take(16 * 1024 + 1)
    .read_to_end(&mut receipt)?;
  let selection = setup::inspect_bundled_ctld(&receipt, false).await?;
  if selection.executable.canonicalize()? != executable.canonicalize()? || !selection.installed {
    return Err(setup::Error::Verification(
      "installed release differs from the selected package".into(),
    ));
  }
  Ok(selection.archive_sha256)
}
