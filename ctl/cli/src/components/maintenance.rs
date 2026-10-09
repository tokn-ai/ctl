//! CLI maintenance delegates mutations to the existing owner-pinned lifecycle APIs.
use clap::ValueEnum;
use ctl_core::component::ComponentInfo;
use ctl_core::protocol::ProtocolVersion;
use serde::Serialize;
use std::io;
use std::path::PathBuf;

mod restart;
mod status;
pub(super) use restart::run as restart;
pub(super) use status::run as status;

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

fn validate_target(
  host: Option<&str>,
  method: Option<&str>,
  platform: Option<crate::RemotePlatform>,
  component: Option<Daemon>,
) -> io::Result<()> {
  if method.is_some() && host.is_none() {
    return Err(io::Error::other("--method requires --host"));
  }
  if matches!(platform, Some(crate::RemotePlatform::Windows)) {
    return Err(io::Error::other(
      "remote component maintenance requires a Unix SSH host",
    ));
  }
  if host.is_some() && component.is_some_and(|component| component != Daemon::Ctmuxd) {
    return Err(io::Error::other(
      "remote restart currently supports only ctmuxd",
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

async fn remote(host: &str, method: Option<&str>) -> io::Result<RemoteConnection> {
  let target = crate::target::resolve(Some(host), method)
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unsupported_remote_restarts_are_rejected_before_connecting() {
    for daemon in [Daemon::Ctld, Daemon::CtlTaskd] {
      assert!(validate_target(Some("work"), None, None, Some(daemon)).is_err());
    }
    assert!(validate_target(Some("work"), None, None, Some(Daemon::Ctmuxd)).is_ok());
    assert!(validate_target(None, Some("vpn"), None, None).is_err());
    assert!(
      validate_target(
        Some("work"),
        None,
        Some(crate::RemotePlatform::Windows),
        None
      )
      .is_err()
    );
  }
}
