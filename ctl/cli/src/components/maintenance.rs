//! CLI maintenance delegates mutations to the existing owner-pinned lifecycle APIs.
use super::TargetOptions;
use clap::ValueEnum;
use ctl_core::component::ComponentInfo;
use ctl_core::protocol::ProtocolVersion;
use serde::Serialize;
use std::io::{self, IsTerminal as _};
use std::path::PathBuf;

mod restart;
mod status;
mod stop;
pub(super) use restart::run as restart;
pub(super) use status::run as status;
pub(super) use stop::run as stop;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Daemon {
  Ctld,
  Ctmuxd,
  CtlTaskd,
}

impl Daemon {
  fn name(self) -> &'static str {
    match self {
      Self::Ctld => "ctld",
      Self::Ctmuxd => "ctmuxd",
      Self::CtlTaskd => "ctl-taskd",
    }
  }

  fn required(self) -> &'static [(&'static str, &'static [ProtocolVersion])] {
    match self {
      Self::Ctld => &[
        ("ctld", ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS),
        (
          "ctld_lifecycle",
          ctl_ipc::lifecycle::SUPPORTED_PROTOCOL_VERSIONS,
        ),
      ],
      Self::Ctmuxd => &[
        ("ctmux", ctmux_proto::SUPPORTED_PROTOCOL_VERSIONS),
        (
          "ctmux_control",
          ctmux_ipc::LOCAL_CONTROL_SUPPORTED_PROTOCOL_VERSIONS,
        ),
      ],
      Self::CtlTaskd => &[
        ("task", ctl_task_proto::SUPPORTED_PROTOCOL_VERSIONS),
        (
          "task_control",
          ctl_task_proto::control::SUPPORTED_PROTOCOL_VERSIONS,
        ),
      ],
    }
  }

  fn has_executable_override(self) -> bool {
    let variable = match self {
      Self::Ctld => "CTLD_BIN",
      Self::Ctmuxd => "CTMUXD_BIN",
      Self::CtlTaskd => "CTL_TASKD_BIN",
    };
    std::env::var_os(variable).is_some()
  }

  fn executable(self) -> io::Result<PathBuf> {
    match self {
      Self::Ctld => ctl_ipc::daemon_executable().map_err(io::Error::other),
      Self::Ctmuxd => ctmux_ipc::daemon_executable().map_err(io::Error::other),
      Self::CtlTaskd => ctl_task_client::daemon_executable().map_err(io::Error::other),
    }
  }

  fn impact(self) -> &'static str {
    match self {
      Self::Ctld => {
        "Disconnects clients using this ctld endpoint and stops its VPN connections. Clients must reconnect; credentials and saved hosts are preserved."
      }
      Self::Ctmuxd => {
        "Ends ALL sessions and panes owned by this ctmuxd endpoint, including other clients and interactive tasks. Session contents cannot be recovered."
      }
      Self::CtlTaskd => {
        "Restarts only an idle task owner. Active tasks cause refusal; task definitions, history and terminal sessions are preserved."
      }
    }
  }
}

fn compatible(component: Daemon, info: &ComponentInfo) -> bool {
  info.is_valid()
    && component.required().iter().all(|(name, versions)| {
      info
        .protocols
        .iter()
        .any(|protocol| protocol.name == *name && protocol.negotiate(versions).is_some())
    })
}

fn validate_target(target: TargetOptions<'_>, component: Option<Daemon>) -> io::Result<()> {
  let TargetOptions {
    host,
    destination,
    method,
    platform,
  } = target;
  if destination.is_some() && host.is_some() {
    return Err(io::Error::other(
      "--destination cannot be combined with --host",
    ));
  }
  if method.is_some() && host.is_none() {
    return Err(io::Error::other("--method requires --host"));
  }
  if matches!(platform, Some(crate::RemotePlatform::Windows)) {
    return Err(io::Error::other(
      "remote component maintenance requires a Unix SSH host",
    ));
  }
  if (host.is_some() || destination.is_some())
    && component.is_some_and(|component| component != Daemon::Ctmuxd)
  {
    return Err(io::Error::other(
      "remote daemon maintenance currently supports only ctmuxd",
    ));
  }
  Ok(())
}

struct RemoteConnection {
  destination: String,
  options: ctl_client::SshConnectionOptions,
  control_path: PathBuf,
  remote_id: String,
}

#[cfg(unix)]
async fn remote(selection: TargetOptions<'_>) -> io::Result<RemoteConnection> {
  let target = crate::target::resolve(selection.host, selection.destination, selection.method)
    .await
    .map_err(io::Error::other)?
    .target;
  if target.is_local() {
    return Err(io::Error::other("--host must select an SSH host"));
  }
  crate::target::ensure_vpn(&target)
    .await
    .map_err(io::Error::other)?;
  let control_path =
    crate::ssh_broker::ensure_master(target.to_ssh_target().map_err(io::Error::other)?)
      .await
      .map_err(io::Error::other)?;
  let ctl_client::ConnectionTarget::Ssh {
    destination,
    options,
  } = target.to_core()
  else {
    unreachable!()
  };
  let identity = ctl_client::maintenance::inspect_agent(&destination, &options, &control_path)
    .await
    .map_err(io::Error::other)?;
  target
    .verify_remote_identity(&identity)
    .map_err(io::Error::other)?;
  Ok(RemoteConnection {
    destination,
    options,
    control_path,
    remote_id: identity.remote_id,
  })
}

#[cfg(not(unix))]
fn remote(_selection: TargetOptions<'_>) -> std::future::Ready<io::Result<RemoteConnection>> {
  std::future::ready(Err(io::Error::new(
    io::ErrorKind::Unsupported,
    "Remote component maintenance requires Unix SSH multiplexing; local component status, stop, and restart are available.",
  )))
}

fn flag(value: Option<bool>) -> &'static str {
  match value {
    Some(true) => "yes",
    Some(false) => "no",
    None => "?",
  }
}
fn build_label(info: Option<&ComponentInfo>) -> String {
  info.map_or_else(
    || "—".into(),
    |info| {
      format!(
        "{} {}{}",
        crate::table::text(&info.build.version),
        info.build.source_fingerprint.get(..8).unwrap_or("?"),
        if info.build.dirty { "*" } else { "" }
      )
    },
  )
}
fn output(value: &impl Serialize) -> io::Result<()> {
  println!(
    "{}",
    serde_json::to_string_pretty(value).map_err(io::Error::other)?
  );
  Ok(())
}

fn require_approval(action: &str, yes: bool, dry_run: bool) -> io::Result<()> {
  if !yes && !dry_run && (!io::stdin().is_terminal() || !io::stderr().is_terminal()) {
    return Err(io::Error::other(format!(
      "{action} requires an interactive terminal or explicit --yes; use --dry-run to inspect the plan"
    )));
  }
  Ok(())
}
async fn confirm(
  action: &'static str,
  yes: bool,
  deadline: tokio::time::Instant,
) -> io::Result<()> {
  if !yes {
    let approved = tokio::task::spawn_blocking(move || {
      cliclack::confirm(format!(
        "{action} this daemon? (Preparation expires after 20 seconds)"
      ))
      .initial_value(false)
      .interact()
    })
    .await
    .map_err(io::Error::other)??;
    if !approved {
      return Err(io::Error::new(
        io::ErrorKind::Interrupted,
        format!("{action} cancelled; no service was changed."),
      ));
    }
  }
  if tokio::time::Instant::now() >= deadline {
    return Err(io::Error::other(
      "Maintenance preparation expired; no service was changed. Run the command again.",
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unsupported_remote_restarts_are_rejected_before_connecting() {
    let local = TargetOptions {
      host: None,
      destination: None,
      method: None,
      platform: None,
    };
    for target in [
      TargetOptions {
        host: Some("work"),
        ..local
      },
      TargetOptions {
        destination: Some("ssh-config-alias"),
        ..local
      },
    ] {
      for daemon in [Daemon::Ctld, Daemon::CtlTaskd] {
        assert!(validate_target(target, Some(daemon)).is_err());
      }
      assert!(validate_target(target, Some(Daemon::Ctmuxd)).is_ok());
      assert!(validate_target(target, None).is_ok());
      assert!(
        validate_target(
          TargetOptions {
            platform: Some(crate::RemotePlatform::Windows),
            ..target
          },
          None,
        )
        .is_err()
      );
    }
    assert!(
      validate_target(
        TargetOptions {
          method: Some("vpn"),
          ..local
        },
        None
      )
      .is_err()
    );
    assert!(
      validate_target(
        TargetOptions {
          destination: Some("ssh-config-alias"),
          method: Some("vpn"),
          ..local
        },
        None
      )
      .is_err()
    );
    assert!(
      validate_target(
        TargetOptions {
          host: Some("work"),
          destination: Some("ssh-config-alias"),
          ..local
        },
        None
      )
      .is_err()
    );
  }
}
