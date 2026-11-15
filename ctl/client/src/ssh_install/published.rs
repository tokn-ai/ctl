//! Run one pinned publication on the SSH host, with an explicit availability fallback.

use super::{
  checkout_layout_guard, require_updater_success, unix_account_guard, updater_ssh_command, windows,
};
use crate::remote_bundle::published::InstallationPlan;
use crate::{CoreError, RemotePlatform, SshConnectionOptions, SshInteraction, validate_ssh_target};
use ctl_core::bundles::Manifest;
use std::{
  io,
  process::Stdio,
  time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

const CHECKING: &[u8] = b"ctl-published-stage-v1 checking-updater\n";
const DOWNLOAD: &[u8] = b"ctl-published-stage-v1 downloading-updater\n";
const UPLOAD: &[u8] = b"ctl-published-stage-v1 uploading-updater\n";
const INSTALLED: &[u8] = b"ctl-published-stage-v1 using-installed-updater\n";
const RUNTIME: &[u8] = b"ctl-published-stage-v1 installing-runtime\n";
const ARCHIVE: &[u8] = b"ctl-published-stage-v1 uploading-runtime\n";
const REUSED: &[u8] = b"ctl-published-reused-v1\n";
const SUCCESS: &[u8] = b"ctl-published-install-v1\n";
const UNAVAILABLE: &[u8] = b"ctl-published-unavailable-v1\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishedInstallStage {
  CheckingLocalCache,
  UsingLocalCache,
  UploadFallback,
  CheckingUpdater,
  DownloadingUpdater,
  UploadingUpdater,
  UploadingUpdaterBytes { sent: u64, total: u64 },
  UsingInstalledUpdater,
  InstallingRuntime,
  UploadingRuntime,
  UploadingRuntimeBytes { sent: u64, total: u64 },
  ReusedRuntime,
}

/// Attempts direct installation of an immutable publication. Only a confirmed
/// downloader availability or old-updater capability miss permits SSH fallback.
///
/// # Errors
/// Returns a hard error for identity, integrity, installation, and SSH failures.
#[allow(clippy::too_many_arguments)]
pub async fn try_install_published_remote(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  expected_account: Option<&str>,
  plan: &InstallationPlan,
  cached_updater: Option<&[u8]>,
  cached_runtime: Option<&[u8]>,
  on_stage: impl Fn(PublishedInstallStage) + Send + Sync,
) -> Result<Option<Manifest>, CoreError> {
  validate_ssh_target(destination, options)?;
  let info = plan.updater_info();
  if cached_updater.is_some_and(|bytes| bytes.len() as u64 != info.size)
    || (cached_updater.is_some() && cached_runtime.is_some())
  {
    return Err(CoreError::InvalidComponentBundle(
      "invalid cached artifacts for remote fetching".into(),
    ));
  }
  let origin = crate::installation::encode_catalog_origin(plan.origin())
    .map_err(|err| CoreError::InvalidComponentBundle(err.to_string()))?;
  let script = script(
    options.remote_platform,
    expected_account,
    &[
      &info.size.to_string(),
      &info.sha256,
      plan.updater_url(),
      &origin,
      plan.archive_url(),
      plan.target(),
      &plan.manifest_bytes().len().to_string(),
      if cached_updater.is_some() { "1" } else { "0" },
      if cached_runtime.is_some() { "1" } else { "0" },
    ],
  )?;
  let command = match options.remote_platform {
    RemotePlatform::Unix => updater_ssh_command(destination, options, interaction, &script).await?,
    RemotePlatform::Windows => {
      windows::windows_ssh_command(destination, options, interaction, &script).await?
    }
  };
  let response = run(
    command,
    plan.manifest_bytes(),
    cached_updater,
    cached_runtime,
    on_stage,
  )
  .await?;
  if let Some(manifest) = &response {
    let source = if info.app_version == info.bundle_id {
      ctl_core::bundles::Source::Release
    } else {
      ctl_core::bundles::Source::Ci
    };
    manifest
      .validate()
      .map_err(|err| CoreError::InvalidComponentBundle(err.to_string()))?;
    if manifest.source != source
      || manifest.target_triple != plan.target()
      || manifest.distribution_id.as_deref() != Some(&info.bundle_id)
      || &manifest.components != plan.components()
    {
      return Err(CoreError::InvalidComponentBundle(
        "remote activation differs from the pinned publication".into(),
      ));
    }
  }
  Ok(response)
}

fn script(
  platform: RemotePlatform,
  account: Option<&str>,
  args: &[&str; 9],
) -> Result<String, CoreError> {
  let (template, guard, assignments) = match platform {
    RemotePlatform::Unix => {
      let quoted = args.map(|arg| format!("'{}'", arg.replace('\'', "'\\''")));
      (
        include_str!("published.sh"),
        unix_account_guard(account)?,
        format!("set -- {}", quoted.join(" ")),
      )
    }
    RemotePlatform::Windows => {
      // PowerShell string literals are quoted here. The helper and downloader
      // additionally pass URL arguments to Windows processes without a shell.
      if args
        .iter()
        .any(|arg| arg.contains(['"', '\\']) || arg.chars().any(char::is_control))
      {
        return Err(CoreError::InvalidComponentBundle(
          "invalid Windows publication arguments".into(),
        ));
      }
      let assignments = [
        "size",
        "sha",
        "url",
        "origin",
        "archive",
        "target",
        "manifestBytes",
        "localUpdater",
        "localRuntime",
      ]
      .into_iter()
      .zip(args)
      .map(|(name, arg)| format!("${name} = '{}'", arg.replace('\'', "''")))
      .collect::<Vec<_>>()
      .join("\n");
      (
        include_str!("published.ps1"),
        windows::account_guard(account)?,
        assignments,
      )
    }
  };
  Ok(
    template
      .replace(
        "__CHECKOUT_LAYOUT_GUARD__",
        &checkout_layout_guard(platform),
      )
      .replace("__ACCOUNT_GUARD__", &guard)
      .replace("__ARGUMENTS__", &assignments),
  )
}

async fn run(
  mut command: tokio::process::Command,
  manifest: &[u8],
  cached_updater: Option<&[u8]>,
  cached_runtime: Option<&[u8]>,
  on_stage: impl Fn(PublishedInstallStage),
) -> Result<Option<Manifest>, CoreError> {
  let mut child = command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .map_err(CoreError::StartSsh)?;
  let mut stdin = child.stdin.take().ok_or(CoreError::MissingSshStdin)?;
  let mut stdout = child.stdout.take().ok_or(CoreError::MissingSshStdout)?;
  let mut stderr = child.stderr.take().ok_or(CoreError::MissingSshStderr)?;
  let (send, mut requests) = tokio::sync::mpsc::unbounded_channel();
  let (activity, mut observed) =
    tokio::sync::watch::channel(PublishedInstallStage::CheckingUpdater);
  let upload_activity = &activity;
  let remote_activity = &activity;
  let on_stage = &on_stage;
  let io = async {
    tokio::join!(
      async move {
        while let Some(stage) = requests.recv().await {
          match stage {
            PublishedInstallStage::UploadingUpdater => {
              write_asset(
                &mut stdin,
                cached_updater.ok_or_else(invalid)?,
                upload_activity,
                stage,
                |sent, total| {
                  on_stage(PublishedInstallStage::UploadingUpdaterBytes { sent, total });
                },
              )
              .await?;
              stdin.flush().await?;
            }
            PublishedInstallStage::InstallingRuntime | PublishedInstallStage::UploadingRuntime => {
              stdin.write_all(manifest).await?;
              if stage == PublishedInstallStage::UploadingRuntime {
                write_asset(
                  &mut stdin,
                  cached_runtime.ok_or_else(invalid)?,
                  upload_activity,
                  stage,
                  |sent, total| {
                    on_stage(PublishedInstallStage::UploadingRuntimeBytes { sent, total });
                  },
                )
                .await?;
              }
              break;
            }
            _ => {}
          }
        }
        Ok::<_, io::Error>(())
      },
      async move {
        let parsed = response(&mut stdout, |stage| {
          remote_activity.send_replace(stage);
          on_stage(stage);
          let _ = send.send(stage);
        })
        .await;
        drop(send);
        // Drain errors as well, so verbose peers cannot block on a full pipe.
        if parsed.is_err() {
          let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
        }
        parsed
      },
      crate::read_bounded_output(&mut stderr),
      child.wait()
    )
  };
  tokio::pin!(io);
  let (write, parsed, diagnostic, status) = loop {
    tokio::select! {
      result = &mut io => break result,
      // Runtime fetching shares one five-minute budget across downloaders.
      // The transport also allows verification and activation after fetching.
      // Healthy uploads can run for as long as needed.
      change = tokio::time::timeout(Duration::from_mins(12), observed.changed()) => {
        match change {
          Ok(Ok(())) => {}
          Ok(Err(_)) => break io.await,
          Err(_) => return Err(CoreError::ReadSshCommand(io::Error::new(io::ErrorKind::TimedOut, "remote publication installation stalled"))),
        }
      }
    }
  };
  let status = status.map_err(CoreError::WaitSshCommand)?;
  let diagnostic = diagnostic.map_err(CoreError::ReadSshCommand)?;
  if status.code() == Some(12) && matches!(parsed, Ok(None)) {
    return Ok(None);
  }
  require_updater_success(status, &[], &diagnostic, &[])?;
  write.map_err(CoreError::WriteSshCommand)?;
  parsed
    .map_err(CoreError::ReadSshCommand)?
    .map(Some)
    .ok_or(CoreError::InvalidSshCommandOutput)
}

async fn write_asset(
  stdin: &mut (impl AsyncWrite + Unpin),
  bytes: &[u8],
  activity: &tokio::sync::watch::Sender<PublishedInstallStage>,
  expected_stage: PublishedInstallStage,
  mut on_bytes: impl FnMut(u64, u64),
) -> io::Result<()> {
  let total = bytes.len() as u64;
  let mut report = |sent| {
    // A receiver marker takes precedence over late sender-side progress.
    if *activity.borrow() == expected_stage {
      on_bytes(sent, total);
    }
  };
  let mut sent = 0;
  let mut last_report = Instant::now();
  report(0);
  for chunk in bytes.chunks(64 * 1024) {
    stdin.write_all(chunk).await?;
    activity.send_modify(|_| {});
    sent += chunk.len() as u64;
    if sent == total || last_report.elapsed() >= Duration::from_millis(250) {
      report(sent);
      last_report = Instant::now();
    }
  }
  Ok(())
}

async fn response(
  input: impl AsyncRead + Unpin,
  on_stage: impl Fn(PublishedInstallStage),
) -> io::Result<Option<Manifest>> {
  use PublishedInstallStage as Stage;

  let mut reader = BufReader::new(input);
  crate::ssh_startup::Preface::default()
    .read_marker(&mut reader, &[CHECKING], b"ctl-published-")
    .await?;
  let mut stage = Stage::CheckingUpdater;
  on_stage(stage);
  let mut line = Vec::new();
  let result = loop {
    line.clear();
    (&mut reader).take(128).read_until(b'\n', &mut line).await?;
    let preparing = matches!(
      stage,
      Stage::DownloadingUpdater | Stage::UploadingUpdater | Stage::UsingInstalledUpdater
    );
    let installing = matches!(stage, Stage::InstallingRuntime | Stage::UploadingRuntime);
    stage = match line.as_slice() {
      DOWNLOAD if stage == Stage::CheckingUpdater => Stage::DownloadingUpdater,
      UPLOAD if stage == Stage::CheckingUpdater => Stage::UploadingUpdater,
      INSTALLED if stage == Stage::CheckingUpdater => Stage::UsingInstalledUpdater,
      RUNTIME if preparing => Stage::InstallingRuntime,
      ARCHIVE if preparing => Stage::UploadingRuntime,
      REUSED if installing => Stage::ReusedRuntime,
      SUCCESS if installing || stage == Stage::ReusedRuntime => {
        line.clear();
        let limit = ctl_core::bundles::MAX_MANIFEST_BYTES + 1;
        (&mut reader)
          .take(limit as u64)
          .read_until(b'\n', &mut line)
          .await?;
        if !line.ends_with(b"\n") {
          return Err(invalid());
        }
        break Some(serde_json::from_slice(&line).map_err(io::Error::other)?);
      }
      UNAVAILABLE if preparing || stage == Stage::InstallingRuntime => break None,
      _ => return Err(invalid()),
    };
    on_stage(stage);
  };
  if reader.read(&mut [0]).await? != 0 {
    return Err(invalid());
  }
  Ok(result)
}

fn invalid() -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidData,
    "invalid remote publication installation response",
  )
}

#[cfg(test)]
mod tests;
