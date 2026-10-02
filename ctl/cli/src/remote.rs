//! One interactive repair attempt, using the already authenticated SSH route.

use std::future::Future;
use std::io::{self, IsTerminal as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ctl_client::hosts::ConnectionTargetDto;
use ctl_client::remote_bundle::{self, Platform, VerifiedBundle};

use ctl_client::{
  CoreError, RemoteInstallEvent, RemoteService, SshConnectionOptions, SshInteraction,
};
use tokio::sync::watch;

mod bundle;

#[derive(Default)]
pub(super) struct Recovery {
  started: AtomicBool,
  repair_started: tokio::sync::Notify,
}

impl Recovery {
  fn begin_connection(&self) -> bool {
    !self.started.swap(true, Ordering::AcqRel)
  }

  pub(super) async fn interrupt(&self) -> io::Result<()> {
    self.repair_started.notified().await;
    // Once registered, Tokio owns SIGINT for this process. Keep this receiver
    // alive through retry and session creation, until the whole command ends.
    tokio::signal::ctrl_c().await
  }

  pub(super) async fn connect<T, E, C, CF, R, RF>(&self, connect: C, repair: R) -> Result<T, E>
  where
    E: From<CoreError>,
    C: Fn() -> CF,
    CF: Future<Output = Result<T, CoreError>>,
    R: FnOnce(CoreError) -> RF,
    RF: Future<Output = Result<(), E>>,
  {
    let initial = self.begin_connection();
    match connect().await {
      Ok(stream) => Ok(stream),
      Err(error) if initial => {
        repair(error).await?;
        connect().await.map_err(Into::into)
      }
      Err(error) => Err(error.into()),
    }
  }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
  #[error(transparent)]
  Core(#[from] CoreError),
  #[error(transparent)]
  Host(#[from] ctl_client::hosts::HostError),
  #[error(transparent)]
  Bundle(#[from] remote_bundle::Error),
  #[error("could not prepare remote repair: {0}")]
  Io(#[from] io::Error),
  #[error("remote repair worker failed: {0}")]
  Worker(#[from] tokio::task::JoinError),
  #[error("Remote repair cancelled.")]
  Cancelled,
  #[error("{0}")]
  Stalled(#[from] ctl_client::RemoteInstallStalled),
  #[error(
    "The saved host identity cannot be verified with this remote agent. Repair it through the desktop's remote setup before reconnecting."
  )]
  IdentityUnavailable,
}

fn repair_reason(error: &CoreError) -> Option<&'static str> {
  match error {
    CoreError::AgentNotFound => Some("The remote ctl-agent is missing."),
    CoreError::IdentityUnsupported => Some("The remote ctl-agent predates host identity support."),
    CoreError::UnsupportedSshProtocol { marker } if marker == "ctl-ssh-v2" => {
      Some("The remote ctl-agent uses the older rmux protocol.")
    }
    _ => None,
  }
}

pub(super) async fn offer_repair(
  error: &CoreError,
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  service: RemoteService,
  settings: &ConnectionTargetDto,
  recovery: &Recovery,
) -> Result<bool, Error> {
  let Some(reason) = repair_reason(error) else {
    return Ok(false);
  };
  // Never consume piped command input or ask during a background reconnect.
  if options.remote_platform != ctl_client::RemotePlatform::Unix
    || !io::stdin().is_terminal()
    || !io::stdout().is_terminal()
    || !io::stderr().is_terminal()
  {
    return Ok(false);
  }
  if matches!(error, CoreError::UnsupportedSshProtocol { .. }) {
    let identity =
      ctl_client::inspect_legacy_ssh_identity(destination, options, interaction, service).await?;
    settings.verify_remote_identity(&identity)?;
  } else if matches!(
    settings,
    ConnectionTargetDto::Ssh {
      remote_info: Some(_),
      ..
    }
  ) {
    // An authenticated SSH account is not a substitute for its pinned machine ID.
    return Err(Error::IdentityUnavailable);
  }
  let prompt = format!(
    "{reason} Install matching remote components on {} and retry?",
    crate::table::text(destination),
  );
  let accepted =
    tokio::task::spawn_blocking(move || cliclack::confirm(prompt).initial_value(false).interact())
      .await?;
  match accepted {
    Ok(true) => {}
    Ok(false) => return Ok(false),
    Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(Error::Cancelled),
    Err(error) => return Err(error.into()),
  }
  recovery.repair_started.notify_one();
  repair(destination, options, interaction).await?;
  eprintln!("ctl: Remote components installed. Retrying the connection.");
  Ok(true)
}

async fn repair(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
) -> Result<(), Error> {
  eprintln!("ctl: Detecting the remote platform...");
  let output = tokio::time::timeout(
    Duration::from_mins(1),
    ctl_client::probe_ssh_unix_platform_interactive(destination, options, interaction),
  )
  .await
  .map_err(|_| ctl_client::RemoteInstallStalled {
    message: "Remote platform detection stopped making progress for 60 seconds.".into(),
  })??;
  let platform = Platform::parse_probe(&output)?;
  let target = platform.target_triple()?;
  eprintln!(
    "ctl: Preparing matching components for {} {}...",
    platform.os, platform.architecture
  );
  let bundle = bundle::matching_bundle(target).await?;
  install(destination, options, interaction, &bundle).await
}

async fn install(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  bundle: &VerifiedBundle,
) -> Result<(), Error> {
  use ctl_client::{RemoteInstallPhase as Phase, RemoteInstallProgress, RemoteInstallWatchdog};
  let initial = RemoteInstallProgress {
    phase: Phase::Connecting,
    file_name: Some(bundle.file_name.clone()),
    total_bytes: bundle.archive.len() as u64,
    ..RemoteInstallProgress::default()
  };
  let (updates, mut receiver) = watch::channel(initial);
  let upload = ctl_client::install_ssh_unix_agent_interactive_with_progress(
    destination,
    options,
    interaction,
    &bundle.bundle_id,
    &bundle.archive,
    |event| {
      updates.send_modify(|progress| match event {
        RemoteInstallEvent::Receiving { received_bytes } => {
          progress.phase = Phase::Transferring;
          progress.transferred_bytes = received_bytes;
        }
        RemoteInstallEvent::Extracting => progress.phase = Phase::Extracting,
        RemoteInstallEvent::Checking { file_name } => {
          progress.phase = Phase::Checking;
          progress.file_name = Some(file_name.into());
        }
        RemoteInstallEvent::Activating => {
          progress.phase = Phase::Activating;
          progress.file_name = None;
        }
        RemoteInstallEvent::Complete => progress.phase = Phase::Complete,
      });
    },
  );
  tokio::pin!(upload);
  let mut watchdog = RemoteInstallWatchdog::new(Instant::now());
  let mut tick = tokio::time::interval(Duration::from_millis(500));
  let mut display = ProgressDisplay::default();
  loop {
    tokio::select! {
      result = &mut upload => {
        display.finish();
        result?;
        return Ok(());
      }
      _ = tick.tick() => {},
      changed = receiver.changed() => { if changed.is_err() { return Err(Error::Cancelled); } }
    }
    let progress = watchdog.observe(receiver.borrow().clone(), Instant::now(), false)?;
    display.show(&progress);
  }
}

#[derive(Default)]
struct ProgressDisplay {
  active: bool,
  phase: Option<ctl_client::RemoteInstallPhase>,
  file_name: Option<String>,
}

impl ProgressDisplay {
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
      if changed {
        eprintln!("ctl: Sending {file}");
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
        Phase::Extracting => "Extracting",
        Phase::Checking => "Checking",
        Phase::Activating => "Activating",
        Phase::Complete => "Installed",
        Phase::Transferring => unreachable!(),
      };
      eprintln!("ctl: {stage} {file}");
      return;
    };
    eprint!("\r\x1b[2Kctl: {message}");
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
mod tests;
