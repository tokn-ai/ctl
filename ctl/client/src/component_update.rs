//! Shared package/source policy for explicit app and CLI component updates.

use ctl_core::bundles::agent::{AgentSource, SOURCE_FILE};
use ctl_core::bundles::{Bundle, MANIFEST_FILE, MAX_FILE_BYTES, Manifest, Purpose, Source, Store};
use ctl_core::installations::{CatalogEntry, CatalogOrigin, Installations};
use std::io;
use std::path::{Path, PathBuf};

mod catalog;
#[cfg(target_os = "macos")]
pub use crate::components::macos_runtime::{
  MacosRuntimeHelper, MacosRuntimeHelperMode, authorize_macos_ad_hoc, macos_runtime_helper_mode,
  replace_macos_helper,
};
pub use catalog::install_prepared_components;
mod remote_publication;
pub use remote_publication::{
  PublicationDeployment, PublicationTarget, deploy_remote_publication, deploy_remote_release,
};

pub use ctl_core::component_update::{
  BuildSource, BundleChannel, Package, UpdateOptions, UpdateResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparationPhase {
  Resolving,
  Building,
  Downloading,
  Verifying,
}

#[derive(Debug, Clone)]
pub struct PreparationProgress {
  pub phase: PreparationPhase,
  pub detail: String,
  pub received_bytes: u64,
  pub total_bytes: u64,
}

impl std::fmt::Display for PreparationProgress {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.detail)
  }
}

fn preparation(phase: PreparationPhase, detail: impl Into<String>) -> PreparationProgress {
  PreparationProgress {
    phase,
    detail: detail.into(),
    received_bytes: 0,
    total_bytes: 0,
  }
}

/// A verified runtime and the exact updater selected during preparation.
pub struct PreparedUpdate {
  pub bundle: Bundle,
  pub origin: Option<CatalogOrigin>,
  source: BuildSource,
  updater: Option<Vec<u8>>,
  updater_info: Option<crate::remote_bundle::published::UpdaterInfo>,
  remote_archive: Option<Vec<u8>>,
  snapshot: PreparationSnapshot,
  #[cfg(target_os = "macos")]
  pub(crate) authorized_ad_hoc_bundle: Option<String>,
  #[cfg(target_os = "macos")]
  require_runtime_signing: bool,
}

/// Installed channel state captured before resolving or building an update.
#[derive(Debug, Clone)]
pub struct PreparationSnapshot {
  entries: Vec<CatalogEntry>,
  started: std::time::Instant,
}

impl PreparationSnapshot {
  /// Captures the account catalog before any source preparation begins.
  ///
  /// # Errors
  /// Reports unsafe storage or malformed installation metadata.
  pub fn capture(home: &Path) -> io::Result<Self> {
    let started = std::time::Instant::now();
    Ok(Self {
      entries: Installations::new(home).catalog_index()?,
      started,
    })
  }
}

impl PreparedUpdate {
  /// Extends conflict detection to an earlier operation boundary, such as a PR
  /// download that happened before preparing this target's archive.
  pub fn retain_earlier_snapshot(&mut self, snapshot: &PreparationSnapshot) {
    if snapshot.started < self.snapshot.started {
      self.snapshot = snapshot.clone();
    }
  }

  /// Retains a manager that supports the local macOS signing workflow on future updates.
  #[cfg(target_os = "macos")]
  pub fn require_macos_runtime_signing(&mut self) {
    self.require_runtime_signing = true;
  }
}

type PreparedSource = (
  Bundle,
  Option<Vec<u8>>,
  Option<CatalogOrigin>,
  Option<crate::remote_bundle::published::UpdaterInfo>,
);
type ImportedSource = (
  Bundle,
  Option<crate::remote_bundle::published::VerifiedUpdater>,
);

/// Prepare a complete target bundle for durable local storage, without
/// constructing an SSH archive or selecting it for native execution.
///
/// # Errors
/// Rejects unavailable, corrupt or incompatible target bundles and updaters.
pub async fn prepare_component(
  home: &Path,
  target: &str,
  source: &BuildSource,
  included: &[PathBuf],
  on_progress: impl Fn(PreparationProgress) + Send + Sync,
) -> io::Result<PreparedUpdate> {
  let snapshot = PreparationSnapshot::capture(home)?;
  let (bundle, mut updater, origin, updater_info) = prepare_source(
    home,
    target,
    Purpose::Upload,
    source,
    included,
    &on_progress,
  )
  .await?;
  verify_source(&bundle.manifest, target, Purpose::Upload)?;
  if updater.is_none() {
    updater = Some(crate::installation::updater_for_bundle(home, &bundle).await?);
  }
  let updater_info = updater_info.or(crate::installation::supplied_updater_info(
    home,
    &bundle.manifest,
  )?);
  Ok(PreparedUpdate {
    bundle,
    origin,
    source: source.clone(),
    updater,
    updater_info,
    remote_archive: None,
    snapshot,
    #[cfg(target_os = "macos")]
    authorized_ad_hoc_bundle: None,
    #[cfg(target_os = "macos")]
    require_runtime_signing: false,
  })
}

pub(crate) fn local_build_origin(bundle: &Bundle) -> io::Result<CatalogOrigin> {
  let build = &bundle.manifest.components["ctl-agent"].build;
  let built_at = time::OffsetDateTime::now_utc()
    .replace_nanosecond(0)
    .map_err(io::Error::other)?
    .format(&time::format_description::well_known::Rfc3339)
    .map_err(io::Error::other)?;
  let origin = CatalogOrigin {
    channel: "local".into(),
    publication_id: Some(format!("local.{}", build.source_fingerprint)),
    built_at: Some(built_at),
  };
  origin.validate()?;
  Ok(origin)
}

fn provided_origin(bundle: &Bundle, path: &Path) -> io::Result<CatalogOrigin> {
  use crate::remote_bundle::{build_info, read_bounded_file};
  let directory = if path.is_dir() {
    path
  } else {
    path
      .parent()
      .ok_or_else(|| invalid("provided build has no parent"))?
  };
  let id = bundle
    .manifest
    .distribution_id
    .as_ref()
    .unwrap_or(&bundle.manifest.bundle_id);
  let build = &bundle.manifest.components["ctl-agent"].build;
  let built_at = match read_bounded_file(
    &directory.join(build_info::FILE_NAME),
    build_info::MAX_BYTES,
  ) {
    Ok(bytes) => Some(
      build_info::BuildInfo::parse_identity(
        &bytes,
        &build.version,
        id,
        build.source_revision.as_deref().unwrap_or_default(),
      )
      .map_err(io::Error::other)?
      .built_at,
    ),
    Err(crate::remote_bundle::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
    Err(error) => return Err(io::Error::other(error)),
  };
  let origin = CatalogOrigin {
    channel: "provided".into(),
    publication_id: Some(id.clone()),
    built_at,
  };
  origin.validate()?;
  Ok(origin)
}

/// Prepare artifacts before starting the remote installation watchdog.
/// # Errors
/// Reports source discovery, compilation, verification and packaging failures.
pub async fn prepare_update(
  home: &Path,
  target: &str,
  purpose: Purpose,
  source: &BuildSource,
  included: &[PathBuf],
  on_progress: impl Fn(PreparationProgress) + Send + Sync,
) -> io::Result<PreparedUpdate> {
  let snapshot = PreparationSnapshot::capture(home)?;
  on_progress(preparation(
    PreparationPhase::Resolving,
    "Resolving component bundle",
  ));
  let prepared = prepare_source(home, target, purpose, source, included, &on_progress).await?;
  prepare_resolved_update(
    home,
    target,
    purpose,
    source,
    prepared,
    snapshot,
    &on_progress,
  )
  .await
}

/// Builds a Linux SSH bundle from the specified checkout and prepares it for upload.
///
/// # Errors
/// Reports an unsupported target, missing checkout, build, verification or packaging failure.
pub async fn prepare_working_tree_update(
  home: &Path,
  target: &str,
  workspace: Option<&Path>,
  on_progress: impl Fn(PreparationProgress) + Send + Sync,
) -> io::Result<PreparedUpdate> {
  let snapshot = PreparationSnapshot::capture(home)?;
  on_progress(preparation(
    PreparationPhase::Resolving,
    "Resolving component bundle",
  ));
  let purpose = Purpose::Upload;
  let prepared = prepare_working_tree(home, target, purpose, workspace, &on_progress).await?;
  prepare_resolved_update(
    home,
    target,
    purpose,
    &BuildSource::WorkingTree,
    prepared,
    snapshot,
    &on_progress,
  )
  .await
}

async fn prepare_resolved_update(
  home: &Path,
  target: &str,
  purpose: Purpose,
  source: &BuildSource,
  (bundle, mut updater, origin, updater_info): PreparedSource,
  snapshot: PreparationSnapshot,
  on_progress: &(impl Fn(PreparationProgress) + Send + Sync),
) -> io::Result<PreparedUpdate> {
  verify_source(&bundle.manifest, target, purpose)?;
  let remote_archive = if purpose == Purpose::Upload {
    if updater.is_none() {
      on_progress(preparation(
        PreparationPhase::Resolving,
        "Resolving the target updater",
      ));
      updater = Some(crate::installation::updater_for_bundle(home, &bundle).await?);
    }
    on_progress(preparation(
      PreparationPhase::Verifying,
      "Verifying and preparing the SSH archive",
    ));
    let snapshot = bundle.clone();
    Some(
      tokio::task::spawn_blocking(move || {
        crate::components::upload_bundle(&snapshot)
          .map(|upload| upload.archive)
          .map_err(io::Error::other)
      })
      .await
      .map_err(io::Error::other)??,
    )
  } else {
    None
  };
  let updater_info = updater_info.or(crate::installation::supplied_updater_info(
    home,
    &bundle.manifest,
  )?);
  Ok(PreparedUpdate {
    bundle,
    origin,
    source: source.clone(),
    updater,
    updater_info,
    remote_archive,
    snapshot,
    #[cfg(target_os = "macos")]
    authorized_ad_hoc_bundle: None,
    #[cfg(target_os = "macos")]
    require_runtime_signing: false,
  })
}

/// Resolves the requested source without replacing running services. Automatic
/// sources use a fresh named publication. Explicit Selected sources use
/// included offline resources without consulting old pins.
/// Local macOS keeps its signed active build because remote archives omit its app.
///
/// # Errors
/// Rejects wrong-target, incomplete, corrupt or incompatible builds.
#[cfg(all(test, unix))]
async fn prepare(
  home: &Path,
  target: &str,
  purpose: Purpose,
  source: &BuildSource,
  included: &[PathBuf],
) -> io::Result<Bundle> {
  let (bundle, _, _, _) = prepare_source(home, target, purpose, source, included, &|_| {}).await?;
  verify_source(&bundle.manifest, target, purpose)?;
  Ok(bundle)
}

async fn prepare_source(
  home: &Path,
  target: &str,
  purpose: Purpose,
  source: &BuildSource,
  included: &[PathBuf],
  on_progress: &(impl Fn(PreparationProgress) + Send + Sync),
) -> io::Result<PreparedSource> {
  if matches!(source, BuildSource::Automatic)
    && purpose == Purpose::Local
    && let Some(prepared) = automatic_native(home, target, on_progress).await?
  {
    return Ok(prepared);
  }
  let published_target = publisher_target(target, purpose);
  if matches!(source, BuildSource::WorkingTree) {
    return prepare_working_tree(home, published_target.as_ref(), purpose, None, on_progress).await;
  }
  if matches!(source, BuildSource::Automatic)
    && (purpose == Purpose::Upload || !target.ends_with("-apple-darwin"))
  {
    let channel = crate::installation::default_channel(home)?;
    return published_build(home, published_target.as_ref(), channel, on_progress).await;
  }
  if let BuildSource::Published { channel } = source {
    return published_build(home, published_target.as_ref(), *channel, on_progress).await;
  }
  if let BuildSource::Release { publication_id } = source {
    let plan = crate::remote_bundle::published::resolve_release_installation(
      publication_id,
      published_target.as_ref(),
      purpose == Purpose::Upload,
    )
    .await
    .map_err(io::Error::other)?;
    return published_plan_source(home, &plan, None, None, on_progress).await;
  }
  if let BuildSource::Provided {
    path,
    local_build: true,
    ctld_package,
  } = source
  {
    if target != ctl_core::paths::native_target() {
      return Err(invalid(
        "native build files cannot be uploaded to another target",
      ));
    }
    on_progress(preparation(
      PreparationPhase::Verifying,
      "Verifying the native build",
    ));
    let bundle = crate::components::import_local(home, path, ctld_package.as_deref()).await?;
    let origin = provided_origin(&bundle, path)?;
    return Ok((bundle, None, Some(origin), None));
  }
  if matches!(source, BuildSource::Automatic) {
    // Included and published macOS runtime archives contain the flat SSH
    // payload, which cannot supply a locally signed helper.
    return Err(invalid(&format!(
      "no compatible signed macOS runtime is available. {SIGNED_MAC_RUNTIME_REBUILD}"
    )));
  }
  on_progress(preparation(
    PreparationPhase::Verifying,
    "Verifying the requested component bundle",
  ));
  let home = home.to_owned();
  let target = target.to_owned();
  let source = source.clone();
  let included = included.to_vec();
  tokio::task::spawn_blocking(move || {
    let (bundle, updater) = match &source {
      BuildSource::Selected => included_build(&home, &included, &target, purpose)?,
      BuildSource::Provided { path, .. } => provided_build(&home, path, &target, purpose)?,
      BuildSource::Automatic
      | BuildSource::Published { .. }
      | BuildSource::Release { .. }
      | BuildSource::WorkingTree => {
        unreachable!(
          "automatic, published and working-tree sources resolved before filesystem preparation"
        )
      }
    };
    verify_source(&bundle.manifest, &target, purpose)?;
    let origin = match &source {
      BuildSource::Provided { path, .. } => Some(provided_origin(&bundle, path)?),
      _ => Installations::new(&home)
        .catalog_index()?
        .into_iter()
        .find(|entry| entry.bundle_id == bundle.manifest.bundle_id && entry.current)
        .and_then(|entry| entry.origin),
    };
    let updater_info = if let Some(updater) = &updater {
      Some(updater.info.clone())
    } else {
      crate::installation::supplied_updater_info(&home, &bundle.manifest)?
    };
    Ok((
      bundle,
      updater.map(|updater| updater.bytes),
      origin,
      updater_info,
    ))
  })
  .await
  .map_err(io::Error::other)?
}

async fn automatic_native(
  home: &Path,
  target: &str,
  on_progress: &(impl Fn(PreparationProgress) + Send + Sync),
) -> io::Result<Option<PreparedSource>> {
  let account = home.to_owned();
  let target = target.to_owned();
  let candidate = tokio::task::spawn_blocking(move || {
    let installations = Installations::new(&account);
    let inspection = installations.inspect_status()?;
    let Some(id) = inspection.active else {
      return Ok(None);
    };
    let origin = inspection.active_origin;
    let pinned = origin
      .as_ref()
      .is_some_and(|origin| !matches!(origin.channel.as_str(), "stable" | "nightly"));
    let active = inspection
      .bundles
      .into_iter()
      .find(|bundle| bundle.manifest.bundle_id == id);
    if let Some(bundle) = active {
      if !pinned && !target.ends_with("-apple-darwin") {
        return Ok(None);
      }
      return Ok(Some((bundle, origin, false)));
    }
    let entry = inspection
      .entries
      .iter()
      .find(|entry| entry.bundle_id == id)
      .ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::WouldBlock,
          "active installation changed; retry the update",
        )
      })?;
    let Some(bundle) = cached_exact(&account, entry)? else {
      if pinned {
        return Err(io::Error::new(
          io::ErrorKind::NotFound,
          "The pinned runtime and its verified cache are missing. Select a source to install.",
        ));
      }
      return Ok(None);
    };
    if bundle.manifest.schema_version != 2
      || verify_source(&bundle.manifest, &target, Purpose::Local).is_err()
      || (target.ends_with("-apple-darwin") && !has_native_helper(&bundle)?)
    {
      return Ok(None);
    }
    Ok::<_, io::Error>(Some((bundle, origin, true)))
  })
  .await
  .map_err(io::Error::other)??;
  let Some((bundle, origin, restoring)) = candidate else {
    return Ok(None);
  };
  on_progress(preparation(
    PreparationPhase::Verifying,
    if restoring {
      "Verifying the cached copy of the active build"
    } else {
      "Verifying the active component bundle"
    },
  ));
  let updater_info = crate::installation::supplied_updater_info(home, &bundle.manifest)?;
  Ok(Some((bundle, None, origin, updater_info)))
}

fn cached_exact(home: &Path, entry: &CatalogEntry) -> io::Result<Option<Bundle>> {
  let cache = Store::cache(home);
  match cache.get(&entry.target_triple, &entry.bundle_id) {
    Ok(bundle) => Ok(Some(bundle)),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error),
  }
}

fn has_native_helper(bundle: &Bundle) -> io::Result<bool> {
  Ok(
    (bundle.manifest.files.contains_key("ctld-package.json")
      && bundle
        .manifest
        .files
        .contains_key("ctld.app/Contents/MacOS/ctld"))
      || bundle.macos_ad_hoc_helper()?,
  )
}

const SIGNED_MAC_RUNTIME_REBUILD: &str = "Run ctlup dev from a source checkout or supply native binaries with --from, --local-build and --ctld-package.";

async fn prepare_working_tree(
  home: &Path,
  target: &str,
  purpose: Purpose,
  workspace: Option<&Path>,
  on_progress: &(impl Fn(PreparationProgress) + Send + Sync),
) -> io::Result<PreparedSource> {
  if !matches!(
    target,
    "x86_64-unknown-linux-musl" | "aarch64-unknown-linux-musl"
  ) {
    return Err(invalid(
      "working-tree bundles require a Linux x64 or ARM64 SSH target; use ctlup dev for native builds",
    ));
  }
  let workspace = if let Some(workspace) = workspace {
    workspace
      .canonicalize()
      .ok()
      .filter(|root| root.join("scripts/dev/build-agent-bundle.mts").is_file())
  } else {
    std::env::current_dir()
      .ok()
      .and_then(|caller| {
        caller
          .ancestors()
          .find(|root| root.join("scripts/dev/build-agent-bundle.mts").is_file())
          .map(Path::to_owned)
      })
      .or_else(development_workspace)
  }
  .ok_or_else(|| invalid("run ctlup dev from a ctl source checkout or pass --workspace"))?;
  development_build(home, target, purpose, &workspace, on_progress).await
}

/// Identifies a debug source checkout without creating or changing any files.
#[must_use]
pub fn development_workspace() -> Option<PathBuf> {
  if !cfg!(debug_assertions) {
    return None;
  }
  let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2)?;
  (workspace.join(".git").exists()
    && workspace
      .join("scripts/dev/build-agent-bundle.mts")
      .is_file())
  .then(|| workspace.to_owned())
}

#[must_use]
pub fn working_tree_available() -> bool {
  development_workspace().is_some()
}

async fn development_build(
  home: &Path,
  target: &str,
  purpose: Purpose,
  workspace: &Path,
  on_progress: &(impl Fn(PreparationProgress) + Send + Sync),
) -> io::Result<PreparedSource> {
  on_progress(preparation(
    PreparationPhase::Building,
    format!("Building {target} from the working tree"),
  ));
  let output = crate::installation::TemporaryDirectory::new("development")?;
  let mut command = tokio::process::Command::new("node");
  command
    .args([
      "--experimental-strip-types",
      "--disable-warning=ExperimentalWarning",
    ])
    .arg(workspace.join("scripts/dev/build-agent-bundle.mts"))
    .args(["--target", target])
    .arg("--output-root")
    .arg(&output.0)
    .env("CARGO_TERM_COLOR", "never")
    .current_dir(workspace);
  run_development_builder(command, on_progress).await?;
  let path = output.0.join(target);
  on_progress(preparation(
    PreparationPhase::Verifying,
    "Verifying the working-tree runtime and updater",
  ));
  let home = home.to_owned();
  let target = target.to_owned();
  tokio::task::spawn_blocking(move || {
    let (bundle, updater) = provided_build(&home, &path, &target, purpose)?;
    let updater = updater.ok_or_else(|| invalid("working-tree build is missing its updater"))?;
    let mut origin = local_build_origin(&bundle)?;
    if let Some(date) = provided_origin(&bundle, &path)?.built_at {
      origin.built_at = Some(date);
    }
    Ok((
      bundle,
      Some(updater.bytes),
      Some(origin),
      Some(updater.info),
    ))
  })
  .await
  .map_err(io::Error::other)?
}

const MAX_BUILD_DIAGNOSTIC_BYTES: usize = 32 * 1024;

async fn run_development_builder(
  mut command: tokio::process::Command,
  on_progress: &(impl Fn(PreparationProgress) + Send + Sync),
) -> io::Result<()> {
  command
    .stdin(std::process::Stdio::piped())
    // The builder prints its output directory. Keep --json stdout exclusively
    // for the caller's structured result while progress remains on stderr.
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .env("CTLUP_BUNDLE_BUILD_LIFELINE", "1");
  let mut child = command.spawn().map_err(|error| {
    io::Error::other(format!(
      "Could not start the development component builder (Node 24 is required): {error}"
    ))
  })?;
  // Closing stdin on cancellation lets the script stop its owned processes.
  let _lifeline = child.stdin.take();
  let reader = child
    .stderr
    .take()
    .ok_or_else(|| io::Error::other("development builder has no diagnostic stream"))?;
  let mut sink = tokio::io::stderr();
  let (status, diagnostics) = tokio::join!(
    child.wait(),
    build_diagnostics(reader, &mut sink, on_progress)
  );
  let status = status?;
  let diagnostics = diagnostics?;
  if !status.success() {
    return Err(io::Error::other(format!(
      "Development component build exited {status}; no components were installed.\n{}",
      diagnostics.trim()
    )));
  }
  Ok(())
}

async fn build_diagnostics(
  mut reader: impl tokio::io::AsyncRead + Unpin,
  sink: &mut (impl tokio::io::AsyncWrite + Unpin),
  on_progress: &(impl Fn(PreparationProgress) + Send + Sync),
) -> io::Result<String> {
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
  let mut tail = Vec::new();
  let mut chunk = [0; 4096];
  let mut line = Vec::new();
  loop {
    let count = reader.read(&mut chunk).await?;
    if count == 0 {
      break;
    }
    let _displayed = sink.write_all(&chunk[..count]).await;
    for &byte in &chunk[..count] {
      if matches!(byte, b'\n' | b'\r') {
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if let Some(detail) = text.strip_prefix("ctl: ").or_else(|| {
          ["Compiling ", "Finished "]
            .iter()
            .any(|prefix| text.starts_with(prefix))
            .then(|| text.split(" (").next().unwrap_or(text))
        }) {
          on_progress(preparation(PreparationPhase::Building, detail));
        }
        line.clear();
      } else if line.len() < 256 {
        line.push(byte);
      }
    }
    tail.extend_from_slice(&chunk[..count]);
    if tail.len() > MAX_BUILD_DIAGNOSTIC_BYTES {
      tail.drain(..tail.len() - MAX_BUILD_DIAGNOSTIC_BYTES);
    }
  }
  Ok(String::from_utf8_lossy(&tail).into_owned())
}

async fn published_build(
  home: &Path,
  target: &str,
  channel: BundleChannel,
  on_progress: &(impl Fn(PreparationProgress) + Send + Sync),
) -> io::Result<PreparedSource> {
  let plan = crate::remote_bundle::published::resolve_target_installation(channel, target)
    .await
    .map_err(io::Error::other)?;
  published_plan_source(home, &plan, None, None, on_progress).await
}

/// Prepares the pinned SSH upload using any already-read cache entries.
///
/// # Errors
/// Rejects cached artifacts from a different publication and reports preparation failures.
pub(crate) async fn prepare_published_upload_with_cached(
  home: &Path,
  plan: &crate::remote_bundle::published::InstallationPlan,
  runtime: Option<crate::remote_bundle::VerifiedBundle>,
  updater: Option<crate::remote_bundle::published::VerifiedUpdater>,
  on_progress: impl Fn(PreparationProgress) + Send + Sync,
) -> io::Result<PreparedUpdate> {
  let snapshot = PreparationSnapshot::capture(home)?;
  let source = if plan.origin().channel == "release" {
    BuildSource::Release {
      publication_id: plan
        .origin()
        .publication_id
        .clone()
        .expect("pinned release ID"),
    }
  } else {
    BuildSource::Published {
      channel: plan.channel(),
    }
  };
  let prepared = published_plan_source(home, plan, runtime, updater, &on_progress).await?;
  prepare_resolved_update(
    home,
    plan.target(),
    Purpose::Upload,
    &source,
    prepared,
    snapshot,
    &on_progress,
  )
  .await
}

async fn published_plan_source(
  home: &Path,
  plan: &crate::remote_bundle::published::InstallationPlan,
  runtime: Option<crate::remote_bundle::VerifiedBundle>,
  updater: Option<crate::remote_bundle::published::VerifiedUpdater>,
  on_progress: &(impl Fn(PreparationProgress) + Send + Sync),
) -> io::Result<PreparedSource> {
  use crate::remote_bundle::published::DownloadArtifact;
  let target = plan.target();
  let report = |progress: crate::remote_bundle::published::DownloadProgress| {
    let artifact = match progress.artifact {
      DownloadArtifact::Runtime => "Runtime bundle",
      DownloadArtifact::Updater => "ctlup",
    };
    on_progress(PreparationProgress {
      phase: PreparationPhase::Downloading,
      detail: format!("{artifact} for {target}"),
      received_bytes: progress.received_bytes,
      total_bytes: progress.total_bytes,
    });
  };
  let cache = ctl_core::paths::updater_directory_for_account(home).join("cache");
  let updater = if let Some(cached) = updater {
    let info = plan.updater_info().clone();
    tokio::task::spawn_blocking(move || info.verify(cached.bytes))
      .await
      .map_err(io::Error::other)?
      .map_err(io::Error::other)?
  } else {
    plan
      .download_updater(&cache.join("updaters"), report)
      .await
      .map_err(io::Error::other)?
  };
  let bundle = if let Some(cached) = runtime {
    let expected = plan.updater_info();
    if cached.manifest != plan.manifest_bytes()
      || cached.bundle_id != expected.bundle_id
      || cached.app_version != expected.app_version
      || cached.git_revision != expected.git_revision
    {
      return Err(invalid(
        "cached runtime differs from the pinned publication",
      ));
    }
    cached
  } else {
    plan
      .download_runtime(&cache.join("downloads"), report)
      .await
      .map_err(io::Error::other)?
  };
  let source = if bundle.bundle_id == bundle.app_version {
    Source::Release
  } else {
    Source::Ci
  };
  let origin = plan.origin().clone();
  let home = home.to_owned();
  let target = target.to_owned();
  on_progress(preparation(
    PreparationPhase::Verifying,
    "Verifying the published installation",
  ));
  // Import only this identified publication. Never read or mutate the upload
  // profile: channel freshness must not be replaced by a sticky old selection.
  tokio::task::spawn_blocking(move || {
    let imported = crate::components::import_remote(&home, &bundle, &target, source)
      .map_err(io::Error::other)?;
    Ok((
      imported,
      Some(updater.bytes),
      Some(origin),
      Some(updater.info),
    ))
  })
  .await
  .map_err(io::Error::other)?
}

fn verify_source(manifest: &Manifest, target: &str, purpose: Purpose) -> io::Result<()> {
  let matching_target = if purpose == Purpose::Local {
    ctl_core::bundles::local_target(target)
      && ctl_core::bundles::local_target(&manifest.target_triple)
  } else {
    manifest.target_triple == target
  };
  if !matching_target || !crate::components::compatible(&manifest.components) {
    return Err(invalid(
      "choose a compatible complete build for this host's target",
    ));
  }
  if purpose == Purpose::Upload {
    require_shared_services(manifest)?;
  }
  Ok(())
}

fn require_shared_services(manifest: &Manifest) -> io::Result<()> {
  if !crate::remote_bundle::compatibility::supports_shared_services(&manifest.components) {
    return Err(invalid(
      "remote uploads require ctl-agent with ctl_transport 1.0.1; choose a newer complete build",
    ));
  }
  Ok(())
}

pub(crate) fn publisher_target(target: &str, purpose: Purpose) -> std::borrow::Cow<'_, str> {
  // Publisher bundles provide portable musl artifacts for GNU Linux clients.
  if purpose == Purpose::Local
    && let Some(arch) = target.strip_suffix("-unknown-linux-gnu")
  {
    format!("{arch}-unknown-linux-musl").into()
  } else {
    target.into()
  }
}

fn included_build(
  home: &Path,
  directories: &[PathBuf],
  target: &str,
  purpose: Purpose,
) -> io::Result<ImportedSource> {
  let target = publisher_target(target, purpose);
  let target = target.as_ref();
  for directory in directories {
    let directories = std::slice::from_ref(directory);
    let candidate = if purpose == Purpose::Upload {
      crate::remote_bundle::read_shared_service_bundle(directories, target)
    } else {
      crate::remote_bundle::read_compatible_bundle(directories, target)
    }
    .map_err(io::Error::other)?;
    let Some(candidate) = candidate else { continue };
    let source = if candidate.bundle_id == candidate.app_version {
      Source::Release
    } else {
      Source::Ci
    };
    let imported = crate::components::import_remote(home, &candidate, target, source)
      .map_err(io::Error::other)?;
    let updater = crate::installation::remember_supplied_updater(home, &imported, directory)?;
    return Ok((imported, updater));
  }
  Err(invalid(
    "no selected or included compatible build; provide build files or select a bundle in About",
  ))
}

fn provided_build(
  home: &Path,
  path: &Path,
  target: &str,
  purpose: Purpose,
) -> io::Result<ImportedSource> {
  if path.is_dir() {
    if path.join(MANIFEST_FILE).exists() {
      let bundle = Bundle::open(path)?;
      verify_source(&bundle.manifest, target, purpose)?;
      let imported = Store::cache(home).publish(&bundle.manifest, &bundle.read_files()?)?;
      let mut updater = crate::installation::remember_supplied_updater(home, &imported, path)?;
      if updater.is_none() && imported.manifest.source == Source::Local {
        // The development builder keeps the runtime tree immutable and puts
        // its manager in a sibling tree under the same output root.
        let directory = path.canonicalize()?;
        if directory.ends_with(&imported.manifest.target_triple)
          && let Some(root) = directory.parent()
        {
          let sidecars = root.join("updaters").join(&imported.manifest.target_triple);
          updater = crate::installation::remember_supplied_updater(home, &imported, &sidecars)?;
        }
      }
      return Ok((imported, updater));
    }
    return included_build(home, &[path.to_owned()], target, purpose);
  }
  let bytes = crate::components::read_input(path, MAX_FILE_BYTES)?;
  let mut files = crate::components::read_archive(&bytes)?;
  if let Some(metadata) = files.remove(MANIFEST_FILE) {
    if metadata.len() > ctl_core::bundles::MAX_MANIFEST_BYTES {
      return Err(invalid("provided manifest exceeds its size limit"));
    }
    let manifest: Manifest = serde_json::from_slice(&metadata).map_err(io::Error::other)?;
    verify_source(&manifest, target, purpose)?;
    let imported = Store::cache(home).publish(&manifest, &files)?;
    let updater = path
      .parent()
      .map(|parent| crate::installation::remember_supplied_updater(home, &imported, parent))
      .transpose()?
      .flatten();
    return Ok((imported, updater));
  }
  // A publisher archive needs its matching bundle-set.json next to it. Verify
  // the named file, rather than accidentally accepting a different sibling.
  let parent = path
    .parent()
    .ok_or_else(|| invalid("provided archive has no parent"))?;
  let target = publisher_target(target, purpose);
  let target = target.as_ref();
  let candidate = crate::remote_bundle::read_compatible_bundle(&[parent.to_owned()], target)
    .map_err(io::Error::other)?
    .ok_or_else(|| invalid("publisher archives need their matching bundle-set.json"))?;
  if candidate.archive != bytes {
    return Err(invalid(
      "provided archive differs from the target named by bundle-set.json",
    ));
  }
  let source = if candidate.bundle_id == candidate.app_version {
    Source::Release
  } else {
    Source::Ci
  };
  let imported =
    crate::components::import_remote(home, &candidate, target, source).map_err(io::Error::other)?;
  let updater = crate::installation::remember_supplied_updater(home, &imported, parent)?;
  Ok((imported, updater))
}

/// Packages only the agent from a verified complete source; retained daemons
/// are represented by installation links, never copied from this new build.
///
/// # Errors
/// Rejects modified source bytes or incompatible contracts.
pub fn agent_archive(bundle: &Bundle) -> io::Result<Vec<u8>> {
  if !crate::components::compatible(&bundle.manifest.components) {
    return Err(invalid("agent source is incompatible with this client"));
  }
  let files = bundle.read_files()?;
  let source = AgentSource {
    schema_version: 1,
    source_bundle: bundle.manifest.clone(),
  };
  let agent_name =
    ctl_core::bundle_layout::executable_name(&bundle.manifest.target_triple, "ctl-agent");
  let agent = files
    .get(agent_name.as_ref())
    .ok_or_else(|| invalid("missing agent binary"))?;
  source.verify(agent)?;
  let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
  let mut archive = tar::Builder::new(encoder);
  crate::components::append(&mut archive, &agent_name, agent, true)?;
  crate::components::append(
    &mut archive,
    SOURCE_FILE,
    &serde_json::to_vec(&source).map_err(io::Error::other)?,
    false,
  )?;
  let bytes = archive.into_inner()?.finish()?;
  if bytes.len() > MAX_FILE_BYTES {
    return Err(invalid("agent archive exceeds its size limit"));
  }
  Ok(bytes)
}

#[cfg(all(test, unix))]
pub(crate) fn inspect_agent_archive(bytes: &[u8]) -> io::Result<AgentSource> {
  let mut files = crate::components::read_archive(bytes)?;
  let metadata = files
    .remove(SOURCE_FILE)
    .ok_or_else(|| invalid("missing agent source"))?;
  if metadata.len() > ctl_core::bundles::MAX_MANIFEST_BYTES || files.len() != 1 {
    return Err(invalid(
      "agent package must contain only its binary and source record",
    ));
  }
  let source: AgentSource = serde_json::from_slice(&metadata).map_err(io::Error::other)?;
  let agent_name =
    ctl_core::bundle_layout::executable_name(&source.source_bundle.target_triple, "ctl-agent");
  source.verify(
    files
      .get(agent_name.as_ref())
      .ok_or_else(|| invalid("missing agent binary"))?,
  )?;
  Ok(source)
}

/// Installs locally without stopping any owner. Local full bundles still pass
/// the platform's signed-helper validation before changing the local profile.
///
/// # Errors
/// Rejects wrong targets, invalid signing or failed atomic activation.
pub async fn install_local(
  home: &Path,
  bundle: &Bundle,
  package: Package,
) -> io::Result<UpdateResult> {
  if !ctl_core::bundles::local_target(&bundle.manifest.target_triple) {
    return Err(invalid("this build cannot run on the local host"));
  }
  if package != Package::FullBundle {
    return Err(invalid(
      "agent-only overlays are no longer installed; use a complete ctlup update",
    ));
  }
  let installations = Installations::new(home);
  let inventory = installations.inspect()?.entries;
  let origin =
    catalog::known_origin(&inventory, bundle).unwrap_or_else(|| catalog::unknown_origin(bundle));
  let expected_heads = crate::installation::single_target_heads(home, bundle, &origin)?;
  crate::components::verify_local(home, bundle).await?;
  crate::installation::provision_manager_with_origin(home, bundle, Some(&origin)).await?;
  let _installation = activate_local(home, bundle, origin, expected_heads).await?;
  Ok(result(bundle, package))
}

/// Install the local runtime and updater selected in preparation.
/// # Errors
/// Reports invalid signing, an incompatible platform or failed activation.
pub async fn install_prepared_local(
  home: &Path,
  prepared: PreparedUpdate,
  package: Package,
) -> io::Result<UpdateResult> {
  if package != Package::FullBundle
    || !ctl_core::bundles::local_target(&prepared.bundle.manifest.target_triple)
  {
    return Err(invalid(
      "local installation requires a complete native build",
    ));
  }
  let target = prepared.bundle.manifest.target_triple.clone();
  install_prepared_components(home, vec![prepared], None, true)
    .await?
    .into_iter()
    .find(|result| result.target_triple == target)
    .ok_or_else(|| invalid("the native component was not installed"))
}

async fn activate_local(
  home: &Path,
  bundle: &Bundle,
  origin: CatalogOrigin,
  expected_heads: Vec<ctl_core::installations::CatalogEntry>,
) -> io::Result<(Bundle, ctl_core::installations::Lease)> {
  let account = home.to_owned();
  let snapshot = bundle.clone();
  let installed = tokio::task::spawn_blocking(move || {
    let installations = ctl_core::installations::Installations::new(&account);
    let installed = installations.activate_and_lease_with_origin_if_unchanged(
      &snapshot,
      &origin,
      &expected_heads,
    )?;
    if let Err(error) = installations.prune_unused() {
      eprintln!("ctlup: installed successfully; cleanup deferred: {error}");
    }
    Ok::<_, io::Error>(installed)
  })
  .await
  .map_err(io::Error::other)??;
  prune_after_update(home).await;
  Ok(installed)
}

#[must_use]
pub fn result(bundle: &Bundle, package: Package) -> UpdateResult {
  UpdateResult {
    package,
    bundle_id: bundle.manifest.bundle_id.clone(),
    target_triple: bundle.manifest.target_triple.clone(),
    services_preserved: true,
  }
}

/// Agent-only activation can be verified on compatible older agents that do
/// not yet recognize the new on-disk source record.
#[must_use]
pub fn agent_matches(
  expected: &ctl_core::component::ComponentInfo,
  actual: &ctl_proto::RemoteIdentity,
) -> bool {
  actual.is_valid()
    && actual.build.as_ref() == Some(&expected.build)
    && ctl_core::component::protocols_match(&expected.protocols, &actual.protocols)
}

/// Uploads the chosen package from a previously verified complete source.
///
/// # Errors
/// Rejects changed bytes, unsupported targets or failed remote activation.
pub async fn install_prepared_remote(
  destination: &str,
  options: &crate::SshConnectionOptions,
  interaction: &crate::SshInteraction,
  prepared: &PreparedUpdate,
  package: Package,
  on_progress: impl Fn(crate::RemoteInstallProgress) + Send + Sync,
) -> Result<UpdateResult, crate::CoreError> {
  install_prepared_remote_for_account(
    destination,
    options,
    interaction,
    prepared,
    package,
    None,
    on_progress,
  )
  .await
}

/// Installs a prepared bundle, checking the account pin in the mutation channel.
///
/// # Errors
/// Reports a changed account identity, invalid bundle, or failed activation.
pub async fn install_prepared_remote_for_account(
  destination: &str,
  options: &crate::SshConnectionOptions,
  interaction: &crate::SshInteraction,
  prepared: &PreparedUpdate,
  package: Package,
  expected_account: Option<&str>,
  on_progress: impl Fn(crate::RemoteInstallProgress) + Send + Sync,
) -> Result<UpdateResult, crate::CoreError> {
  use crate::{RemoteInstallEvent as Event, RemoteInstallPhase as Phase, RemoteInstallProgress};
  use std::sync::atomic::{AtomicU64, Ordering};
  let invalid = |error: io::Error| crate::CoreError::InvalidComponentBundle(error.to_string());
  if package != Package::FullBundle {
    return Err(invalid(invalid_data(
      "agent-only overlays are no longer installed; use a complete ctlup update",
    )));
  }
  let bundle = &prepared.bundle;
  let home =
    dirs::home_dir().ok_or_else(|| invalid(invalid_data("home directory is unavailable")))?;
  let (Some(updater), Some(archive)) = (&prepared.updater, &prepared.remote_archive) else {
    return Err(invalid(invalid_data(
      "prepare a remote component update before installing",
    )));
  };
  let transferred = AtomicU64::new(0);
  let report_stage = |phase, file_name| {
    on_progress(RemoteInstallProgress {
      phase,
      file_name,
      ..RemoteInstallProgress::default()
    });
  };
  report_stage(Phase::Connecting, None);
  let report = |event| {
    let (phase, file_name) = match event {
      Event::CheckingRemoteInstallation => {
        return report_stage(Phase::CheckingRemoteInstallation, None);
      }
      Event::SendingUpdater {
        sent_bytes,
        total_bytes,
      } => {
        on_progress(RemoteInstallProgress {
          phase: Phase::Transferring,
          file_name: Some("ctlup".into()),
          transferred_bytes: sent_bytes,
          total_bytes,
          bytes_per_second: 0,
        });
        return;
      }
      Event::CheckingUpdater => return report_stage(Phase::Checking, Some("ctlup".into())),
      Event::Reused => return report_stage(Phase::Reused, None),
      Event::Receiving { received_bytes } => {
        transferred.store(received_bytes, Ordering::Relaxed);
        (Phase::Transferring, None)
      }
      Event::Extracting => (Phase::Extracting, None),
      Event::Checking { file_name } => (Phase::Checking, Some(file_name.to_owned())),
      Event::Activating => (Phase::Activating, None),
      Event::Complete => (Phase::Complete, None),
    };
    on_progress(RemoteInstallProgress {
      phase,
      file_name,
      transferred_bytes: transferred.load(Ordering::Relaxed),
      total_bytes: archive.len() as u64,
      bytes_per_second: 0,
    });
  };
  {
    let id = bundle
      .manifest
      .distribution_id
      .as_ref()
      .unwrap_or(&bundle.manifest.bundle_id);
    let upload = crate::ssh_install::UpdaterUpload {
      origin: prepared.origin.as_ref(),
      updater,
      bundle_id: id,
      archive,
      expected_account,
    };
    crate::ssh_install::install_ssh_bundle_with_updater(
      destination,
      options,
      interaction,
      upload,
      report,
    )
    .await?;
  }
  prune_after_update(&home).await;
  Ok(result(bundle, package))
}

fn invalid_data(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) async fn prune_after_update(home: &Path) {
  let home = home.to_owned();
  let result = tokio::task::spawn_blocking(move || crate::installation::prune_caches(&home)).await;
  match result {
    Ok(Ok(_)) => {}
    Ok(Err(error)) => eprintln!("ctlup: installed successfully; cache cleanup deferred: {error}"),
    Err(error) => eprintln!("ctlup: installed successfully; cache cleanup interrupted: {error}"),
  }
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(all(test, unix))]
mod tests;
