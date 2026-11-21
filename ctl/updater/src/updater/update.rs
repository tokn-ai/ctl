use super::{TargetOptions, arguments::InstallArguments};
use ctl_client::component_update::{
  BuildSource, BundleChannel, Package, PreparationPhase, PreparationProgress, PreparedUpdate,
  UpdateResult,
};
use ctl_client::installation::{self, RuntimeSource};
use ctl_client::remote_bundle::{pr, published};
use ctl_core::bundles::{Purpose, Store};
use ctl_core::installations::CatalogOrigin;
use serde::Serialize;
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Serialize)]
struct HostResult {
  host: String,
  result: Option<UpdateResult>,
  #[serde(skip_serializing_if = "Option::is_none")]
  check: Option<ChannelCheck>,
  error: Option<String>,
}

enum Outcome {
  Installed(UpdateResult),
  Checked(ChannelCheck),
}

#[derive(Serialize)]
struct ChannelCheck {
  channel: Option<BundleChannel>,
  active: Option<String>,
  installed_publication: Option<String>,
  available_publication: Option<String>,
  state: CheckState,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckState {
  Uninstalled,
  Pinned,
  Missing,
  Unavailable,
  Current,
  UpdateAvailable,
}

impl CheckState {
  fn description(self) -> &'static str {
    match self {
      Self::Uninstalled => "No runtime installed. Run ctlup install.",
      Self::Pinned => "The runtime is pinned. Use ctlup install to select a new source.",
      Self::Missing => "The active runtime is missing.",
      Self::Unavailable => "The tracked channel has no compatible runtime for this target.",
      Self::Current => "The tracked runtime is current.",
      Self::UpdateAvailable => "An update is available.",
    }
  }
}

enum Operation<'a> {
  Install(InstallSource<'a>),
  Update,
  Check,
}

enum InstallSource<'a> {
  Artifacts {
    source: &'a BuildSource,
    pr: Option<&'a mut PrBuilds>,
  },
  Checkout {
    workspace: &'a Path,
  },
}

impl<'a> InstallSource<'a> {
  fn publication(&self) -> Option<Publication<'a>> {
    match self {
      Self::Artifacts {
        source: BuildSource::Published { channel },
        pr: None,
      } => Some(Publication::Channel(*channel)),
      Self::Artifacts {
        source: BuildSource::Release { publication_id },
        pr: None,
      } => Some(Publication::Release(publication_id)),
      _ => None,
    }
  }
}

enum Publication<'a> {
  Channel(BundleChannel),
  Release(&'a str),
}

struct PrBuilds {
  number: u64,
  unix: Option<pr::DownloadedPrBundle>,
  windows: Option<pr::DownloadedPrBundle>,
}

impl PrBuilds {
  async fn source(&mut self, target: &str) -> io::Result<BuildSource> {
    let cached = if target == ctl_client::remote_bundle::WINDOWS_TARGET {
      &mut self.windows
    } else {
      &mut self.unix
    };
    if cached.is_none() {
      eprintln!(
        "ctlup: Downloading the current build for PR #{} ({target})…",
        self.number
      );
      *cached = Some(
        pr::download_for_target(self.number, target)
          .await
          .map_err(io::Error::other)?,
      );
    }
    Ok(BuildSource::Provided {
      path: cached.as_ref().unwrap().directory().to_owned(),
      local_build: false,
      ctld_package: None,
    })
  }
}

pub(super) async fn install(
  home: &Path,
  options: &InstallArguments,
  targets: &[TargetOptions<'_>],
) -> io::Result<()> {
  let mut pr_builds = options.pr.map(|number| PrBuilds {
    number,
    unix: None,
    windows: None,
  });
  let source = if let Some(path) = options.from.clone() {
    BuildSource::Provided {
      path,
      local_build: options.local_build,
      ctld_package: options.ctld_package.clone(),
    }
  } else if let Some(publication_id) = options.release.clone() {
    BuildSource::Release { publication_id }
  } else if let Some(channel) = options.channel {
    BuildSource::Published {
      channel: channel.into(),
    }
  } else if let Some(path) =
    std::env::var_os("CTL_REMOTE_BUNDLES_DIR").filter(|_| pr_builds.is_none())
  {
    BuildSource::Provided {
      path: path.into(),
      local_build: false,
      ctld_package: None,
    }
  } else {
    BuildSource::Published {
      channel: BundleChannel::default(),
    }
  };
  run_batch(
    home,
    targets,
    Operation::Install(InstallSource::Artifacts {
      source: &source,
      pr: pr_builds.as_mut(),
    }),
    options.json,
  )
  .await
}

pub(super) async fn run(
  home: &Path,
  targets: &[TargetOptions<'_>],
  check: bool,
  json: bool,
) -> io::Result<()> {
  let operation = if check {
    Operation::Check
  } else {
    Operation::Update
  };
  run_batch(home, targets, operation, json).await
}

pub(super) async fn run_development(
  home: &Path,
  targets: &[TargetOptions<'_>],
  workspace: &Path,
  json: bool,
) -> io::Result<()> {
  run_batch(
    home,
    targets,
    Operation::Install(InstallSource::Checkout { workspace }),
    json,
  )
  .await
}

async fn run_batch(
  home: &Path,
  targets: &[TargetOptions<'_>],
  mut operation: Operation<'_>,
  json: bool,
) -> io::Result<()> {
  if targets.iter().any(TargetOptions::remote) {
    crate::bundled::register()?;
  }
  let mut results = Vec::new();
  for &target in targets {
    results.push(run_host(home, target, &mut operation).await?);
  }
  if json {
    super::print_json(&results)?;
  } else {
    for result in &results {
      if let Some(check) = &result.check {
        println!("{}: {}", result.host, check.state.description());
      }
    }
  }
  if results.iter().any(|result| result.error.is_some()) {
    return Err(io::Error::other(
      "the operation failed on some hosts; successful hosts were kept",
    ));
  }
  Ok(())
}

async fn run_host(
  home: &Path,
  target: TargetOptions<'_>,
  operation: &mut Operation<'_>,
) -> io::Result<HostResult> {
  let label = target.label();
  let action = match operation {
    Operation::Install(InstallSource::Checkout { .. }) => "Building for",
    Operation::Install(_) => "Installing on",
    Operation::Update => "Updating",
    Operation::Check => "Checking",
  };
  eprintln!("ctlup: {action} {label}…");
  let outcome = tokio::select! {
    result = update_host(home, target, operation) => result,
    _ = tokio::signal::ctrl_c() => return Err(io::Error::new(io::ErrorKind::Interrupted, "Operation cancelled. Check status before retrying.")),
  };
  let mut response = HostResult {
    host: label.to_owned(),
    result: None,
    check: None,
    error: None,
  };
  match outcome {
    Ok(Outcome::Installed(result)) => {
      eprintln!("ctlup: {label}: installed; running services preserved. Restart separately.");
      response.result = Some(result);
    }
    Ok(Outcome::Checked(check)) => response.check = Some(check),
    Err(error) => {
      eprintln!("ctlup: {label}: {error}");
      response.error = Some(error.to_string());
    }
  }
  Ok(response)
}

struct Endpoint {
  remote: Option<super::remote::Target>,
  triple: &'static str,
  purpose: Purpose,
}

impl Endpoint {
  async fn connect(selection: TargetOptions<'_>) -> io::Result<Self> {
    let resolved = selection.resolve().await?;
    if resolved.is_local() {
      return Ok(Self {
        remote: None,
        triple: ctl_core::paths::native_target(),
        purpose: Purpose::Local,
      });
    }
    let remote = super::remote::Target::connect(resolved).await?;
    remote.verify_account().await?;
    let triple = remote.target_triple().await?;
    Ok(Self {
      remote: Some(remote),
      triple,
      purpose: Purpose::Upload,
    })
  }

  async fn runtime_source(&self, home: &Path) -> io::Result<RuntimeSource> {
    let Some(remote) = &self.remote else {
      return installation::runtime_source(home);
    };
    installation::remote_runtime_source(
      &remote.destination,
      &remote.options,
      &remote.interaction,
      remote.expected_account(),
    )
    .await
    .map_err(io::Error::other)
  }
}

async fn update_host(
  home: &Path,
  selection: TargetOptions<'_>,
  operation: &mut Operation<'_>,
) -> io::Result<Outcome> {
  // Capture local source heads before resolving SSH, so preparation still
  // rejects a newer installation made while the connection was in flight.
  let snapshot = if matches!(operation, Operation::Check) {
    None
  } else {
    Some(ctl_client::component_update::PreparationSnapshot::capture(
      home,
    )?)
  };
  let endpoint = Endpoint::connect(selection).await?;
  if let Operation::Check = operation {
    return check(&endpoint.runtime_source(home).await?)
      .await
      .map(Outcome::Checked);
  }
  let snapshot = snapshot.expect("install and update capture a source snapshot");
  if let Operation::Install(source) = operation {
    return install_selected(home, &endpoint, source, &snapshot, None)
      .await
      .map(Outcome::Installed);
  }

  let tracked = endpoint.runtime_source(home).await?;
  let result = if let Some(channel) = super::tracked_channel(tracked.origin.as_ref()) {
    let source = BuildSource::Published { channel };
    install_selected(
      home,
      &endpoint,
      &mut InstallSource::Artifacts {
        source: &source,
        pr: None,
      },
      &snapshot,
      None,
    )
    .await?
  } else if tracked.missing {
    restore_pinned(home, &endpoint, tracked, &snapshot).await?
  } else if tracked.active.is_some() {
    return check(&tracked).await.map(Outcome::Checked);
  } else {
    return Err(io::Error::other(
      "no runtime is installed; run ctlup install first",
    ));
  };
  Ok(Outcome::Installed(result))
}

async fn check(tracked: &RuntimeSource) -> io::Result<ChannelCheck> {
  let channel = super::tracked_channel(tracked.origin.as_ref());
  let installed_publication = tracked
    .origin
    .as_ref()
    .and_then(|origin| origin.publication_id.clone());
  let (available_publication, compatible) = if let Some(channel) = channel {
    let catalog = published::check(channel).await.map_err(io::Error::other)?;
    let compatible = tracked.target_triple.as_deref().is_none_or(|triple| {
      catalog
        .targets
        .iter()
        .any(|target| target.target_triple == triple && target.compatible)
    });
    (catalog.publication_id, compatible)
  } else {
    (None, false)
  };
  let state = if tracked.active.is_none() {
    CheckState::Uninstalled
  } else if tracked.missing {
    CheckState::Missing
  } else if channel.is_none() {
    CheckState::Pinned
  } else if !compatible {
    CheckState::Unavailable
  } else if installed_publication == available_publication && installed_publication.is_some() {
    CheckState::Current
  } else {
    CheckState::UpdateAvailable
  };
  Ok(ChannelCheck {
    channel,
    active: tracked.active.clone(),
    installed_publication,
    available_publication,
    state,
  })
}

async fn restore_pinned(
  home: &Path,
  endpoint: &Endpoint,
  tracked: RuntimeSource,
  snapshot: &ctl_client::component_update::PreparationSnapshot,
) -> io::Result<UpdateResult> {
  let Some(active) = tracked.active else {
    return Err(io::Error::other(
      "no runtime is installed; run ctlup install first",
    ));
  };
  let target = tracked.target_triple.as_deref().unwrap_or(endpoint.triple);
  let bundle = Store::cache(home).get(target, &active).map_err(|error| {
    if error.kind() == io::ErrorKind::NotFound {
      io::Error::other("the pinned runtime is missing and its exact cache entry is unavailable; reinstall from its original source")
    } else {
      error
    }
  })?;
  let source = BuildSource::Provided {
    path: bundle.directory.clone(),
    local_build: false,
    ctld_package: None,
  };
  let origin = tracked.origin.unwrap_or(CatalogOrigin {
    channel: "provided".into(),
    publication_id: bundle.manifest.distribution_id.clone(),
    built_at: None,
  });
  install_selected(
    home,
    endpoint,
    &mut InstallSource::Artifacts {
      source: &source,
      pr: None,
    },
    snapshot,
    Some((&active, origin)),
  )
  .await
}

async fn install_selected(
  home: &Path,
  endpoint: &Endpoint,
  source: &mut InstallSource<'_>,
  snapshot: &ctl_client::component_update::PreparationSnapshot,
  restore: Option<(&str, CatalogOrigin)>,
) -> io::Result<UpdateResult> {
  let last_download = Mutex::new(None);
  let report = |progress| report_preparation(&progress, &last_download);
  if let Some(remote) = &endpoint.remote
    && let Some(publication) = source.publication()
  {
    return update_remote_publication(home, remote, endpoint.triple, publication, report).await;
  }
  let (mut prepared, pr_number) =
    prepare(home, endpoint.triple, endpoint.purpose, source, report).await?;
  prepared.retain_earlier_snapshot(snapshot);
  if let Some((active, origin)) = restore {
    if prepared.bundle.manifest.bundle_id != active {
      return Err(io::Error::other(
        "the cached runtime differs from the recorded active build",
      ));
    }
    prepared.origin = Some(origin);
  }
  if let Some(number) = pr_number {
    let origin = prepared.origin.get_or_insert_with(|| CatalogOrigin {
      channel: "provided".into(),
      publication_id: prepared.bundle.manifest.distribution_id.clone(),
      built_at: None,
    });
    origin.channel = format!("pr-{number}");
    origin.validate()?;
  }
  let package = Package::FullBundle;
  let Some(remote) = &endpoint.remote else {
    return ctl_client::component_update::install_prepared_local(home, prepared, package).await;
  };
  let result = install_with_progress(remote, &prepared, package).await?;
  verify_remote_install(remote, &prepared.bundle.manifest).await?;
  Ok(result)
}

async fn update_remote_publication(
  home: &Path,
  remote: &super::remote::Target,
  target: &str,
  publication: Publication<'_>,
  report: impl Fn(PreparationProgress) + Send + Sync,
) -> io::Result<UpdateResult> {
  use ctl_client::component_update::{
    PublicationDeployment, PublicationTarget, deploy_remote_publication, deploy_remote_release,
  };
  let target = PublicationTarget {
    destination: &remote.destination,
    options: &remote.options,
    interaction: &remote.interaction,
    expected_account: remote.expected_account(),
    target_triple: target,
  };
  let last_upload = Mutex::new(None);
  let stage = |stage| report_publication(stage, &last_upload);
  let deployed = match publication {
    Publication::Channel(channel) => {
      deploy_remote_publication(home, &target, channel, stage, report).await?
    }
    Publication::Release(id) => deploy_remote_release(home, &target, id, stage, report).await?,
  };
  let package = Package::FullBundle;
  let manifest = match deployed {
    PublicationDeployment::Installed(manifest) => manifest,
    PublicationDeployment::Upload(prepared) => {
      install_with_progress(remote, &prepared, package).await?;
      prepared.bundle.manifest
    }
  };
  verify_remote_install(remote, &manifest).await?;
  Ok(UpdateResult {
    package,
    bundle_id: manifest.bundle_id,
    target_triple: manifest.target_triple,
    services_preserved: true,
  })
}

async fn verify_remote_install(
  remote: &super::remote::Target,
  manifest: &ctl_core::bundles::Manifest,
) -> io::Result<()> {
  remote.verify_active_manifest(manifest).await?;
  if remote.pinned()
    && matches!(
      remote.interaction,
      ctl_client::SshInteraction::Multiplexed { .. }
    )
  {
    remote.verify_activation(manifest).await?;
  }
  Ok(())
}

fn report_publication(
  stage: ctl_client::PublishedInstallStage,
  last_upload: &Mutex<Option<Instant>>,
) {
  use ctl_client::PublishedInstallStage as Stage;
  let message = match stage {
    Stage::CheckingLocalCache => "Checking the publication and local cache",
    Stage::UsingLocalCache => "Using the verified updater and runtime from the local cache",
    Stage::UploadFallback => "Switching to local preparation and upload over SSH",
    Stage::CheckingUpdater => "Checking ctlup on the remote host",
    Stage::DownloadingUpdater => "Downloading ctlup on the remote host with curl or wget",
    Stage::UploadingUpdater => "Uploading cached ctlup over SSH",
    Stage::UploadingUpdaterBytes { sent, total } => {
      return report_bytes("ctlup over SSH", sent, total, last_upload);
    }
    Stage::UsingInstalledUpdater => "Using the verified ctlup already on the remote host",
    Stage::InstallingRuntime => {
      "Checking the remote runtime and downloading it there with curl or wget if needed"
    }
    Stage::UploadingRuntime => "Uploading the cached runtime over SSH",
    Stage::UploadingRuntimeBytes { sent, total } => {
      return report_bytes("Runtime bundle over SSH", sent, total, last_upload);
    }
    Stage::ReusedRuntime => "Reusing the verified runtime already installed on the remote host",
  };
  eprintln!("ctlup: {message}");
}

fn report_preparation(
  progress: &PreparationProgress,
  last_download_report: &Mutex<Option<Instant>>,
) {
  match progress.phase {
    // Cargo's build diagnostics already go to stderr.
    PreparationPhase::Building => {}
    PreparationPhase::Downloading => {
      let received = progress.received_bytes;
      let total = progress.total_bytes;
      if received == 0 && total == 0 {
        eprintln!("ctlup: Downloading {} on this computer", progress.detail);
        *last_download_report.lock().unwrap() = Some(Instant::now());
      } else {
        report_bytes(&progress.detail, received, total, last_download_report);
      }
    }
    _ => eprintln!("ctlup: {progress}"),
  }
}

fn report_bytes(label: &str, bytes: u64, total: u64, last: &Mutex<Option<Instant>>) {
  let mut last = last.lock().unwrap();
  if bytes == 0
    || (bytes != total && last.is_some_and(|last| last.elapsed() < Duration::from_secs(1)))
  {
    return;
  }
  if total == 0 {
    eprintln!("ctlup: {label} ({bytes} bytes)");
  } else {
    eprintln!("ctlup: {label} ({bytes}/{total} bytes)");
  }
  *last = Some(Instant::now());
}

async fn prepare(
  home: &Path,
  target: &str,
  purpose: Purpose,
  source: &mut InstallSource<'_>,
  on_progress: impl Fn(PreparationProgress) + Send + Sync,
) -> io::Result<(PreparedUpdate, Option<u64>)> {
  let (prepared, pr_number) = match source {
    InstallSource::Artifacts { source, pr } => {
      let pr_number = pr.as_ref().map(|build| build.number);
      let pr_source = if let Some(pr) = pr {
        Some(pr.source(target).await?)
      } else {
        None
      };
      let prepared = ctl_client::component_update::prepare_update(
        home,
        target,
        purpose,
        pr_source.as_ref().unwrap_or(source),
        &[],
        on_progress,
      )
      .await?;
      (prepared, pr_number)
    }
    InstallSource::Checkout { workspace } => {
      if purpose != Purpose::Upload {
        return Err(io::Error::other(
          "working-tree bundles require a Linux x64 or ARM64 SSH target",
        ));
      }
      let prepared = ctl_client::component_update::prepare_working_tree_update(
        home,
        target,
        Some(workspace),
        on_progress,
      )
      .await?;
      (prepared, None)
    }
  };
  if purpose == Purpose::Upload {
    let manifest = &prepared.bundle.manifest;
    eprintln!(
      "ctlup: Source bundle {} · revision {}",
      manifest.bundle_id,
      manifest.components["ctl-agent"]
        .build
        .source_revision
        .as_deref()
        .unwrap_or("unknown")
    );
  }
  Ok((prepared, pr_number))
}

async fn install_with_progress(
  remote: &super::remote::Target,
  prepared: &PreparedUpdate,
  package: Package,
) -> io::Result<UpdateResult> {
  let (updates, mut receiver) = tokio::sync::watch::channel(ctl_client::RemoteInstallProgress {
    phase: ctl_client::RemoteInstallPhase::VerifyingBundle,
    ..ctl_client::RemoteInstallProgress::default()
  });
  let upload = ctl_client::component_update::install_prepared_remote_for_account(
    &remote.destination,
    &remote.options,
    &remote.interaction,
    prepared,
    package,
    remote.expected_account(),
    |progress| {
      updates.send_replace(progress);
    },
  );
  tokio::pin!(upload);
  let mut watchdog = ctl_client::RemoteInstallWatchdog::new(Instant::now());
  let mut tick = tokio::time::interval(Duration::from_millis(500));
  let mut display = ProgressDisplay::default();
  loop {
    tokio::select! {
      result = &mut upload => {
        if result.is_ok() {
          display.show(&receiver.borrow());
        }
        return result.map_err(io::Error::other);
      },
      _ = tick.tick() => {},
      changed = receiver.changed() => {
        if changed.is_err() {
          return upload.await.map_err(io::Error::other);
        }
      }
    }
    let progress = watchdog
      .observe(receiver.borrow().clone(), Instant::now(), false)
      .map_err(io::Error::other)?;
    display.show(&progress);
  }
}

#[derive(Default)]
struct ProgressDisplay {
  active: bool,
  phase: Option<ctl_client::RemoteInstallPhase>,
  file_name: Option<String>,
  last_render: Option<Instant>,
  last_rendered_bytes: u64,
}

impl ProgressDisplay {
  fn transfer_due(
    &mut self,
    progress: &ctl_client::RemoteInstallProgress,
    changed: bool,
    now: Instant,
  ) -> bool {
    let completed = progress.transferred_bytes >= progress.total_bytes
      && progress.transferred_bytes != self.last_rendered_bytes;
    if !changed
      && !completed
      && self
        .last_render
        .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_millis(500))
    {
      return false;
    }
    self.last_render = Some(now);
    self.last_rendered_bytes = progress.transferred_bytes;
    true
  }

  fn show(&mut self, progress: &ctl_client::RemoteInstallProgress) {
    use ctl_client::RemoteInstallPhase as Phase;
    let file = crate::table::text(progress.file_name.as_deref().unwrap_or("remote components"));
    let changed = self.phase != Some(progress.phase) || self.file_name != progress.file_name;
    if changed {
      self.finish();
      self.phase = Some(progress.phase);
      self.file_name.clone_from(&progress.file_name);
    }
    let message = if progress.phase == Phase::Transferring {
      // Receiver reports may arrive for every small block. Keep observing them
      // for the watchdog, but bound redraws and redirected diagnostic volume.
      if !self.transfer_due(progress, changed, Instant::now()) {
        return;
      }
      if changed {
        eprintln!("ctlup: Sending {file}");
      }
      let total = progress.total_bytes.max(1);
      let filled = usize::try_from(progress.transferred_bytes.saturating_mul(20) / total)
        .unwrap_or(20)
        .min(20);
      format!(
        "[{}{}] {}% ({}/{} bytes, {} B/s)",
        "=".repeat(filled),
        " ".repeat(20 - filled),
        progress.transferred_bytes.saturating_mul(100) / total,
        progress.transferred_bytes,
        progress.total_bytes,
        progress.bytes_per_second
      )
    } else {
      if !changed {
        return;
      }
      let stage = match progress.phase {
        Phase::DetectingPlatform => "Detecting platform",
        Phase::VerifyingBundle => "Verifying bundle",
        Phase::Connecting => "Opening upload channel",
        Phase::CheckingRemoteInstallation => "Checking installed ctlup and",
        Phase::Extracting => "Extracting",
        Phase::Checking => "Checking",
        Phase::Activating => "Activating",
        Phase::Complete => "Installed",
        Phase::Reused => "Reused installed",
        Phase::Transferring => unreachable!(),
      };
      eprintln!("ctlup: {stage} {file}");
      return;
    };
    eprint!("\r\x1b[2Kctlup: {message}");
    let _ = io::stderr().flush();
    self.active = true;
  }

  fn finish(&mut self) {
    if self.active {
      eprintln!();
      self.active = false;
    }
  }
}

impl Drop for ProgressDisplay {
  fn drop(&mut self) {
    self.finish();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn remote_publication_uses_the_requested_release_identity_only_for_published_sources() {
    fn selected(source: &BuildSource) -> Option<Publication<'_>> {
      InstallSource::Artifacts { source, pr: None }.publication()
    }
    assert!(matches!(
      selected(&BuildSource::Published {
        channel: BundleChannel::Stable
      }),
      Some(Publication::Channel(BundleChannel::Stable))
    ));
    assert!(matches!(
      selected(&BuildSource::Release {
        publication_id: "release-1".into()
      }),
      Some(Publication::Release("release-1"))
    ));
    let workspace = Path::new("test-profile");
    for source in [
      BuildSource::Automatic,
      BuildSource::Selected,
      BuildSource::Provided {
        path: workspace.into(),
        local_build: false,
        ctld_package: None,
      },
    ] {
      assert!(selected(&source).is_none());
    }
    let mut pr = PrBuilds {
      number: 1,
      unix: None,
      windows: None,
    };
    let pr_source = InstallSource::Artifacts {
      source: &BuildSource::Automatic,
      pr: Some(&mut pr),
    };
    assert!(pr_source.publication().is_none());
    assert!(
      InstallSource::Checkout { workspace }
        .publication()
        .is_none()
    );
  }

  #[test]
  fn transfer_redraws_are_bounded_without_delaying_completion_or_phase_changes() {
    let start = Instant::now();
    let mut display = ProgressDisplay::default();
    let mut progress = ctl_client::RemoteInstallProgress {
      phase: ctl_client::RemoteInstallPhase::Transferring,
      total_bytes: 10_000,
      ..ctl_client::RemoteInstallProgress::default()
    };
    let mut rendered = 0;
    for tick in 0..10_000 {
      progress.transferred_bytes = tick;
      rendered += usize::from(display.transfer_due(
        &progress,
        tick == 0,
        start + Duration::from_micros(tick * 100),
      ));
    }
    assert_eq!(rendered, 2);
    progress.transferred_bytes = progress.total_bytes;
    assert!(display.transfer_due(&progress, false, start + Duration::from_secs(1)));
    assert!(!display.transfer_due(&progress, false, start + Duration::from_secs(1)));
    progress.transferred_bytes = 0;
    assert!(display.transfer_due(&progress, true, start + Duration::from_secs(1)));
  }

  #[test]
  fn default_local_and_explicit_batch_targets_are_distinct() {
    let local = super::super::target_options(&[], None, None, None).unwrap();
    assert_eq!(local[0].label(), "local");
    assert!(!local[0].remote());

    let hosts = ["a".into(), "b".into(), "a".into()];
    let batch = super::super::target_options(&hosts, None, None, None).unwrap();
    assert_eq!(
      batch.iter().map(TargetOptions::label).collect::<Vec<_>>(),
      ["a", "b"]
    );
    assert!(batch.iter().all(TargetOptions::remote));
  }

  #[test]
  fn temporary_destination_is_not_a_saved_host() {
    let targets = super::super::target_options(
      &[],
      Some("ssh://user@windows.example:2022"),
      None,
      Some(crate::RemotePlatform::Windows),
    )
    .unwrap();
    assert_eq!(targets[0].host, None);
    assert_eq!(
      targets[0].destination,
      Some("ssh://user@windows.example:2022")
    );
  }
}
