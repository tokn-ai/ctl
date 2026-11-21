use super::{TargetOptions, arguments::UpdateArguments, source_with_bundle_override};
use ctl_client::component_update::{
  BuildSource, BundleChannel, Package, PreparationPhase, PreparationProgress, PreparedUpdate,
  UpdateResult,
};
use ctl_client::remote_bundle::pr;
use ctl_core::bundles::Purpose;
use serde::Serialize;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Serialize)]
struct HostResult {
  host: String,
  result: Option<UpdateResult>,
  error: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Choice {
  Local,
  Saved(String),
  Destination(String),
}

enum UpdateSource<'a> {
  Artifacts {
    source: &'a BuildSource,
    pr: Option<&'a mut PrBuilds>,
  },
  Checkout {
    workspace: &'a Path,
  },
}

impl UpdateSource<'_> {
  fn publication_channel(&self, home: &Path) -> io::Result<Option<BundleChannel>> {
    let Self::Artifacts { source, pr: None } = self else {
      return Ok(None);
    };
    Ok(match source {
      BuildSource::Published { channel } => Some(*channel),
      BuildSource::Automatic => Some(ctl_client::installation::default_channel(home)?),
      _ => None,
    })
  }
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

impl Choice {
  fn host(&self) -> Option<&str> {
    match self {
      Self::Saved(host) => Some(host),
      _ => None,
    }
  }

  fn destination(&self) -> Option<&str> {
    match self {
      Self::Destination(destination) => Some(destination),
      _ => None,
    }
  }
}

pub(super) async fn run(
  home: &Path,
  options: UpdateArguments,
  target_options: TargetOptions<'_>,
) -> io::Result<()> {
  let mut pr_builds = options.pr.map(|number| PrBuilds {
    number,
    unix: None,
    windows: None,
  });
  let explicit_channel = options.channel.map(Into::into);
  let source = if let Some(path) = options.from {
    BuildSource::Provided {
      path,
      local_build: options.local_build,
      ctld_package: options.ctld_package,
    }
  } else if let Some(channel) = explicit_channel {
    BuildSource::Published { channel }
  } else {
    BuildSource::Automatic
  };
  let source = source_with_bundle_override(
    source,
    std::env::var_os("CTL_REMOTE_BUNDLES_DIR").map(PathBuf::from),
  );
  run_batch(
    home,
    target_options,
    options.hosts,
    options.local,
    UpdateSource::Artifacts {
      source: &source,
      pr: pr_builds.as_mut(),
    },
    options.json,
  )
  .await?;
  if let Some(channel) = explicit_channel {
    ctl_client::installation::set_channel(home, channel)?;
  }
  Ok(())
}

pub(super) async fn run_development(
  home: &Path,
  target_options: TargetOptions<'_>,
  hosts: Vec<String>,
  workspace: &Path,
  json: bool,
) -> io::Result<()> {
  run_batch(
    home,
    target_options,
    hosts,
    false,
    UpdateSource::Checkout { workspace },
    json,
  )
  .await
}

async fn run_batch(
  home: &Path,
  target_options: TargetOptions<'_>,
  hosts: Vec<String>,
  local: bool,
  mut source: UpdateSource<'_>,
  json: bool,
) -> io::Result<()> {
  crate::bundled::register()?;
  let TargetOptions {
    host,
    destination,
    method,
    platform,
  } = target_options;
  if method.is_some() && host.is_none() {
    return Err(io::Error::other(
      "--method requires --host; --hosts uses each host's preferred method",
    ));
  }
  let choices = choices(hosts, local, host, destination)?;
  let mut results = Vec::new();
  for choice in choices {
    let selected =
      choice.host().is_some_and(|name| Some(name) == host) || choice.destination().is_some();
    let target = TargetOptions {
      host: choice.host(),
      destination: choice.destination(),
      method: if selected { method } else { None },
      platform: if selected { platform } else { None },
    };
    results.push(run_host(home, target, &mut source).await?);
  }
  if json {
    println!(
      "{}",
      serde_json::to_string(&results).map_err(io::Error::other)?
    );
  }
  if results.iter().any(|result| result.error.is_some()) {
    return Err(io::Error::other(
      "some component updates failed; successful hosts were kept",
    ));
  }
  Ok(())
}

async fn run_host(
  home: &Path,
  target: TargetOptions<'_>,
  source: &mut UpdateSource<'_>,
) -> io::Result<HostResult> {
  let label = target.host.or(target.destination).unwrap_or("local");
  if matches!(source, UpdateSource::Checkout { .. }) {
    eprintln!("ctlup: Building and updating {label} (full bundle)…");
  } else {
    eprintln!("ctlup: Updating {label} (full bundle)…");
  }
  let outcome = tokio::select! {
    result = update_host(home, target, source) => result,
    _ = tokio::signal::ctrl_c() => return Err(io::Error::new(io::ErrorKind::Interrupted, "Update cancelled. Refresh component status before retrying; an activation may already have completed.")),
  };
  let (result, error) = match outcome {
    Ok(result) => {
      eprintln!("ctlup: {label}: installed; running services preserved. Restart separately.");
      (Some(result), None)
    }
    Err(error) => {
      eprintln!("ctlup: {label}: {error}");
      (None, Some(error.to_string()))
    }
  };
  Ok(HostResult {
    host: label.to_owned(),
    result,
    error,
  })
}

fn choices(
  hosts: Vec<String>,
  local: bool,
  host: Option<&str>,
  destination: Option<&str>,
) -> io::Result<Vec<Choice>> {
  if destination.is_some() && (host.is_some() || !hosts.is_empty()) {
    return Err(io::Error::other(
      "--destination cannot be combined with --host or --hosts",
    ));
  }
  let mut choices = Vec::new();
  if let Some(host) = host {
    choices.push(Choice::Saved(host.to_owned()));
  }
  if let Some(destination) = destination {
    choices.push(Choice::Destination(destination.to_owned()));
  }
  for host in hosts {
    let choice = Choice::Saved(host);
    if !choices.contains(&choice) {
      choices.push(choice);
    }
  }
  if local || choices.is_empty() {
    choices.push(Choice::Local);
  }
  Ok(choices)
}

async fn update_host(
  home: &Path,
  selection: TargetOptions<'_>,
  source: &mut UpdateSource<'_>,
) -> io::Result<UpdateResult> {
  let snapshot = ctl_client::component_update::PreparationSnapshot::capture(home)?;
  let mut resolved =
    crate::target::resolve(selection.host, selection.destination, selection.method)
      .await
      .map_err(io::Error::other)?;
  crate::target::apply_remote_platform(&mut resolved.target, selection.platform);
  let remote = if resolved.target.is_local() {
    None
  } else {
    let remote = super::remote::Target::connect(resolved.target).await?;
    remote.verify_account().await?;
    Some(remote)
  };
  let (triple, purpose) = if let Some(remote) = &remote {
    (remote.target_triple().await?, Purpose::Upload)
  } else {
    (ctl_core::paths::native_target(), Purpose::Local)
  };
  let last_download = Mutex::new(None);
  let report = |progress| report_preparation(&progress, &last_download);
  if let Some(remote) = &remote
    && let Some(channel) = source.publication_channel(home)?
  {
    return update_remote_publication(home, remote, triple, channel, report).await;
  }
  let (mut prepared, pr_number) = prepare(home, triple, purpose, source, report).await?;
  prepared.retain_earlier_snapshot(&snapshot);
  if let Some(number) = pr_number {
    let origin = prepared
      .origin
      .get_or_insert_with(|| ctl_core::installations::CatalogOrigin {
        channel: "provided".into(),
        publication_id: prepared.bundle.manifest.distribution_id.clone(),
        built_at: None,
      });
    origin.channel = format!("pr-{number}");
    origin.validate()?;
  }
  let package = Package::FullBundle;
  let Some(remote) = remote else {
    return ctl_client::component_update::install_prepared_local(home, prepared, package).await;
  };
  let result = install_with_progress(&remote, &prepared, package).await?;
  if remote.pinned() {
    remote.verify_activation(&prepared.bundle.manifest).await?;
  }
  Ok(result)
}

async fn update_remote_publication(
  home: &Path,
  remote: &super::remote::Target,
  target: &str,
  channel: BundleChannel,
  report: impl Fn(PreparationProgress) + Send + Sync,
) -> io::Result<UpdateResult> {
  use ctl_client::component_update::{
    PublicationDeployment, PublicationTarget, deploy_remote_publication,
  };
  let target = PublicationTarget {
    destination: &remote.destination,
    options: &remote.options,
    interaction: &remote.interaction,
    expected_account: remote.expected_account(),
    target_triple: target,
  };
  let last_upload = Mutex::new(None);
  let deployed = deploy_remote_publication(
    home,
    &target,
    channel,
    |stage| report_publication(stage, &last_upload),
    report,
  )
  .await?;
  let package = Package::FullBundle;
  let manifest = match deployed {
    PublicationDeployment::Installed(manifest) => manifest,
    PublicationDeployment::Upload(prepared) => {
      install_with_progress(remote, &prepared, package).await?;
      prepared.bundle.manifest
    }
  };
  remote.verify_active_manifest(&manifest).await?;
  if remote.pinned()
    && matches!(
      remote.interaction,
      ctl_client::SshInteraction::Multiplexed { .. }
    )
  {
    remote.verify_activation(&manifest).await?;
  }
  Ok(UpdateResult {
    package,
    bundle_id: manifest.bundle_id,
    target_triple: manifest.target_triple,
    services_preserved: true,
  })
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
  source: &mut UpdateSource<'_>,
  on_progress: impl Fn(PreparationProgress) + Send + Sync,
) -> io::Result<(PreparedUpdate, Option<u64>)> {
  let (prepared, pr_number) = match source {
    UpdateSource::Artifacts { source, pr } => {
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
        &bundle_directories(home)?,
        on_progress,
      )
      .await?;
      (prepared, pr_number)
    }
    UpdateSource::Checkout { workspace } => {
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

fn bundle_directories(home: &Path) -> io::Result<Vec<PathBuf>> {
  if let Some(directory) = std::env::var_os("CTL_REMOTE_BUNDLES_DIR") {
    return Ok(vec![PathBuf::from(directory)]);
  }
  let mut directories = Vec::new();
  if cfg!(debug_assertions) {
    directories.push(
      PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/desktop/src-tauri/resources/agent-bundles"),
    );
  }
  if let Some(parent) = std::env::current_exe()?.parent() {
    directories.push(parent.join("resources/agent-bundles"));
  }
  directories.push(ctl_core::paths::directory_for_account(home).join("agent-bundles"));
  Ok(directories)
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
  fn remote_publication_uses_the_requested_channel_only_for_published_sources() {
    let home = std::env::temp_dir().join(format!("ctlup-remote-source-{}", uuid::Uuid::new_v4()));
    ctl_core::private_fs::create_directory(&home).unwrap();
    ctl_client::installation::set_channel(&home, BundleChannel::Nightly).unwrap();
    let channel = |source: &BuildSource| {
      UpdateSource::Artifacts { source, pr: None }
        .publication_channel(&home)
        .unwrap()
    };
    assert_eq!(
      channel(&BuildSource::Automatic),
      Some(BundleChannel::Nightly)
    );
    assert_eq!(
      channel(&BuildSource::Published {
        channel: BundleChannel::Stable
      }),
      Some(BundleChannel::Stable)
    );
    for source in [
      BuildSource::Selected,
      BuildSource::Provided {
        path: home.clone(),
        local_build: false,
        ctld_package: None,
      },
    ] {
      assert_eq!(channel(&source), None);
    }
    let mut pr = PrBuilds {
      number: 1,
      unix: None,
      windows: None,
    };
    let pr_source = UpdateSource::Artifacts {
      source: &BuildSource::Automatic,
      pr: Some(&mut pr),
    };
    assert_eq!(pr_source.publication_channel(&home).unwrap(), None);
    assert_eq!(
      UpdateSource::Checkout { workspace: &home }
        .publication_channel(&home)
        .unwrap(),
      None
    );
    std::fs::remove_dir_all(home).unwrap();
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
    assert_eq!(
      choices(vec![], false, None, None).unwrap(),
      vec![Choice::Local]
    );
    assert_eq!(
      choices(
        vec!["a".into(), "b".into(), "a".into()],
        true,
        Some("a"),
        None,
      )
      .unwrap(),
      vec![
        Choice::Saved("a".into()),
        Choice::Saved("b".into()),
        Choice::Local
      ]
    );
  }

  #[test]
  fn temporary_destination_can_include_local_without_becoming_a_saved_host() {
    for destination in ["192.0.2.1", "server.example.com", "ssh-config-alias"] {
      let targets = choices(vec![], true, None, Some(destination)).unwrap();
      assert_eq!(
        targets,
        vec![Choice::Destination(destination.into()), Choice::Local]
      );
      assert_eq!(targets[0].host(), None);
      assert_eq!(targets[0].destination(), Some(destination));
    }
  }

  #[test]
  fn temporary_destination_rejects_saved_host_selections() {
    assert!(choices(vec![], false, Some("work"), Some("server")).is_err());
    assert!(choices(vec!["work".into()], false, None, Some("server")).is_err());
  }
}
