use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use ctl_ipc::lifecycle::{Client, DaemonBinaryInfo, DaemonStatus};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncReadExt as _;

use super::models::{
  ComponentAction, ComponentVersionInfo, ComponentVersionRow, ProtocolVersion, VersionStatus,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Owner {
  pub id: String,
  pub label: String,
  pub socket: PathBuf,
  pub executable: Result<PathBuf, String>,
}

impl Owner {
  pub fn client(&self) -> Result<Client, String> {
    Ok(Client::new(self.socket.clone()).with_daemon_executable(self.executable.clone()?))
  }
}

pub(super) fn owners() -> Vec<Owner> {
  let ssh = Owner {
    id: String::new(),
    label: "ctld (SSH)".into(),
    socket: ctl_ipc::socket_path(),
    executable: ctl_ipc::daemon_executable().map_err(|error| error.to_string()),
  };
  let (socket, executable) = crate::vpn::owner_endpoint();
  let vpn = Owner {
    id: String::new(),
    label: "ctld (VPN)".into(),
    socket,
    executable: executable
      .and_then(|value| value.map_or_else(ctl_ipc::daemon_executable, Ok))
      .map_err(|error| error.to_string()),
  };
  deduplicate_owners(ssh, vpn)
}

fn normalized_endpoint(path: &Path) -> PathBuf {
  if let Ok(path) = path.canonicalize() {
    return path;
  }
  let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
  path
    .parent()
    .and_then(|parent| parent.canonicalize().ok())
    .zip(path.file_name())
    .map_or_else(|| path.clone(), |(parent, name)| parent.join(name))
}

fn deduplicate_owners(mut ssh: Owner, mut vpn: Owner) -> Vec<Owner> {
  ssh.socket = normalized_endpoint(&ssh.socket);
  vpn.socket = normalized_endpoint(&vpn.socket);
  for owner in [&mut ssh, &mut vpn] {
    owner.id = format!(
      "ctld:{:x}",
      Sha256::digest(owner.socket.as_os_str().as_encoded_bytes())
    );
  }
  if ssh.socket == vpn.socket {
    ssh.label = "ctld (SSH, VPN)".into();
    vec![ssh]
  } else {
    vec![ssh, vpn]
  }
}

pub(super) fn ctld_version(binary: DaemonBinaryInfo) -> ComponentVersionInfo {
  ComponentVersionInfo::from_build(
    binary.build,
    binary
      .protocols
      .into_iter()
      .map(ProtocolVersion::from)
      .collect(),
  )
}

pub(super) async fn ctld(mut owner: Owner) -> ComponentVersionRow {
  let mut row = ComponentVersionRow::local("ctld", &owner.label);
  row.component_id = owner.id.clone();
  let client = Client::new(owner.socket.clone());
  // This discovers an existing verified helper only; it never installs a
  // payload or replaces the owner observed by the separate passive probe.
  let (running, available) = tokio::join!(client.probe(), async {
    owner.executable = crate::daemon_helper::executable()
      .await
      .map_err(|error| error.to_string());
    owner
      .client()?
      .available()
      .await
      .map_err(|error| error.to_string())
  });
  let replacement_supported = available.as_ref().is_ok_and(|available| {
    available
      .info
      .supports("ctld", ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS)
      && available.info.supports(
        "ctld_lifecycle",
        ctl_ipc::lifecycle::SUPPORTED_PROTOCOL_VERSIONS,
      )
  });
  match available {
    Ok(available) => {
      row.available = Some(ctld_version(available.info));
      if !replacement_supported {
        row.error = Some("The available ctld helper uses an incompatible protocol. Update or rebuild it together with this app before restarting.".into());
      }
    }
    Err(error) => {
      if let Ok(executable) = owner.executable.clone() {
        row.available = read_binary(executable, "ctld").await.ok();
      }
      row.error = Some(format!("Available helper: {error}"));
    }
  }
  match running {
    Ok(DaemonStatus::Absent) => {
      row.status = VersionStatus::NotRunning;
      row.detail = Some("This owner is not running. Opening About does not start it.".into());
    }
    Ok(DaemonStatus::Legacy { protocol_version }) => {
      row.running = Some(ComponentVersionInfo::default());
      row.compare();
      row.detail = Some(protocol_version.map_or_else(
        || "This running ctld predates published contracts and safe restart. Restart it manually to upgrade.".into(),
        |build| format!("This running ctld uses unpublished protocol build {build} and predates safe restart. Restart it manually to upgrade."),
      ));
    }
    Ok(DaemonStatus::Running { info }) => {
      row.running = Some(ctld_version(info.binary));
      row.restart_supported = replacement_supported;
      row.action = replacement_supported.then_some(ComponentAction::Restart);
      row.compare();
    }
    Err(error) => {
      row.status = VersionStatus::Unavailable;
      append_error(&mut row, error.to_string());
    }
  }
  row.note_available_mismatch();
  row.note_unreported_build();
  with_purpose(row, "Connection broker for SSH, port forwards, and VPNs.")
}

fn with_purpose(mut row: ComponentVersionRow, purpose: &str) -> ComponentVersionRow {
  row.detail = Some(match row.detail.take() {
    Some(detail) => format!("{purpose} {detail}"),
    None => purpose.into(),
  });
  row
}

fn append_error(row: &mut ComponentVersionRow, error: String) {
  row.error = Some(match row.error.take() {
    Some(previous) => format!("{previous}; {error}"),
    None => error,
  });
}

pub(super) async fn ctmuxd() -> ComponentVersionRow {
  let mut row = ComponentVersionRow::local("ctmuxd", "ctmuxd");
  let (running, available) = tokio::join!(ctmux_ipc::component_status(), async {
    read_binary(
      ctmux_ipc::daemon_executable().map_err(|error| error.to_string())?,
      "ctmuxd",
    )
    .await
  });
  set_available(&mut row, available);
  match running {
    Ok(Some(info)) => {
      row.restart_supported = info.restart_supported && compatible_replacement(&row);
      row.action = row.restart_supported.then_some(ComponentAction::Restart);
      let protocols = info
        .protocols
        .into_iter()
        .map(ProtocolVersion::from)
        .collect();
      row.running = Some(match info.build {
        Some(build) => ComponentVersionInfo::from_build(build, protocols),
        None => ComponentVersionInfo {
          version: info.version,
          protocols,
          ..ComponentVersionInfo::default()
        },
      });
      row.compare();
      if info.protocol_mismatch {
        row.status = VersionStatus::Incompatible;
        row.detail = Some("The running ctmuxd does not support this app's protocol requirements. Update or restart the daemon with a matching helper.".into());
      }
    }
    Ok(None) => row.status = VersionStatus::NotRunning,
    Err(error) => {
      row.status = VersionStatus::Unavailable;
      append_error(&mut row, error.to_string());
    }
  }
  row.note_available_mismatch();
  row.note_unreported_build();
  with_purpose(row, "Terminal sessions.")
}

pub(super) async fn taskd() -> ComponentVersionRow {
  let mut row = ComponentVersionRow::local("ctl-taskd", "ctl-taskd");
  let (running, available) = tokio::join!(ctl_task_ipc::component_status(), async {
    read_binary(
      ctl_task_client::daemon_executable().map_err(|error| error.to_string())?,
      "ctl-taskd",
    )
    .await
  });
  set_available(&mut row, available);
  match running {
    Ok(Some(info)) => {
      // Older ctl-taskd versions do not implement ComponentStatus, but can still
      // reject a cooperative restart safely when busy or unsupported.
      row.restart_supported = compatible_replacement(&row);
      row.action = row.restart_supported.then_some(ComponentAction::Restart);
      let protocols = info
        .protocols
        .into_iter()
        .map(ProtocolVersion::from)
        .collect();
      row.running = Some(match info.build {
        Some(build) => ComponentVersionInfo::from_build(build, protocols),
        None => ComponentVersionInfo {
          protocols,
          ..ComponentVersionInfo::default()
        },
      });
      row.compare();
      if info.protocol_mismatch {
        row.status = VersionStatus::Incompatible;
        row.detail = Some("The running ctl-taskd does not support this app's protocol requirements. Update or restart the daemon with a matching helper.".into());
      }
    }
    Ok(None) => row.status = VersionStatus::NotRunning,
    Err(error) => {
      row.status = VersionStatus::Unavailable;
      append_error(&mut row, error.to_string());
    }
  }
  row.note_available_mismatch();
  row.note_unreported_build();
  with_purpose(row, "Task execution.")
}

fn compatible_replacement(row: &ComponentVersionRow) -> bool {
  row.available.as_ref().is_some_and(|available| {
    available.source_fingerprint.is_some()
      && row.required_protocols.iter().all(|required| {
        available.protocols.iter().any(|protocol| {
          protocol.name == required.name
            && protocol
              .supported_versions
              .iter()
              .any(|version| required.supported_versions.contains(version))
        })
      })
  })
}

fn set_available(row: &mut ComponentVersionRow, available: Result<ComponentVersionInfo, String>) {
  match available {
    Ok(info) => row.available = Some(info),
    Err(error) => row.error = Some(format!("Available helper: {error}")),
  }
}

async fn read_binary(executable: PathBuf, component: &str) -> Result<ComponentVersionInfo, String> {
  if let Ok(output) = binary_output(&executable, "--component-info").await
    && let Ok(info) = parse_binary_info(&output, component)
  {
    return Ok(info);
  }
  let output = binary_output(&executable, "--version").await?;
  legacy_binary_version(&output, component)
}

fn parse_binary_info(output: &[u8], component: &str) -> Result<ComponentVersionInfo, String> {
  let info: ctl_core::component::ComponentInfo = serde_json::from_slice(output)
    .map_err(|_| "The selected helper returned invalid component metadata")?;
  let protocol_name = match component {
    "ctld" => "ctld",
    "ctmuxd" => "ctmux",
    "ctl-taskd" => "task",
    _ => return Err("Unknown helper component".into()),
  };
  if !info.is_valid()
    || info
      .protocols
      .iter()
      .filter(|protocol| protocol.name == protocol_name)
      .count()
      != 1
  {
    return Err("The selected helper does not report valid metadata for this component".into());
  }
  Ok(ComponentVersionInfo::from_build(
    info.build,
    info
      .protocols
      .into_iter()
      .map(ProtocolVersion::from)
      .collect(),
  ))
}

fn legacy_binary_version(output: &[u8], component: &str) -> Result<ComponentVersionInfo, String> {
  let text = std::str::from_utf8(output).map_err(|_| "Invalid helper version text")?;
  let mut fields = text.split_whitespace();
  let name = fields.next();
  let version = fields.next().ok_or("Missing helper version")?;
  if name != Some(component) || fields.next().is_some() || semver::Version::parse(version).is_err()
  {
    return Err("The selected helper returned an unrecognized version".into());
  }
  Ok(ComponentVersionInfo {
    version: Some(version.into()),
    ..ComponentVersionInfo::default()
  })
}

async fn binary_output(executable: &Path, argument: &str) -> Result<Vec<u8>, String> {
  let result = tokio::time::timeout(Duration::from_secs(3), async {
    let mut child = tokio::process::Command::new(executable)
      .arg(argument)
      .env_remove("CTLD_ASKPASS")
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .map_err(|error| error.to_string())?;
    let mut output = Vec::new();
    let mut stdout = child
      .stdout
      .take()
      .ok_or("Missing helper version output")?
      .take(16_385);
    stdout
      .read_to_end(&mut output)
      .await
      .map_err(|error| error.to_string())?;
    if output.len() > 16_384 {
      return Err("Component metadata exceeded the size limit".into());
    }
    if !child
      .wait()
      .await
      .map_err(|error| error.to_string())?
      .success()
    {
      return Err("The selected helper does not support version reporting".into());
    }
    Ok(output)
  })
  .await;
  result.unwrap_or_else(|_| Err("Component metadata query timed out".into()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(target_os = "macos")]
  #[tokio::test]
  async fn shared_helper_available_child() {
    if std::env::var("CTMUX_HELPER_TEST_MODE").as_deref() != Ok("about") {
      return;
    }
    ctl_ipc::register_daemon_executable_provider(crate::daemon_helper::tests::provider).unwrap();
    let owner = owners().remove(0);
    let row = ctld(owner).await;
    assert!(row.available.is_some(), "{:?}", row.error);
    assert!(row.error.is_none(), "{:?}", row.error);
    assert_eq!(row.status, VersionStatus::NotRunning);
    assert_eq!(
      crate::daemon_helper::executable().await.unwrap(),
      crate::daemon_helper::tests::shared_executable()
    );
  }

  #[cfg(target_os = "macos")]
  #[tokio::test]
  async fn shared_helper_timeout_preserves_running_status_child() {
    if std::env::var("CTMUX_HELPER_TEST_MODE").as_deref() != Ok("about_timeout") {
      return;
    }
    ctl_ipc::register_daemon_executable_provider(crate::daemon_helper::tests::provider).unwrap();
    let owner = owners().remove(0);
    let listener = tokio::net::UnixListener::bind(&owner.socket).unwrap();
    let row = tokio::spawn(ctld(owner));
    // The passive inspection must begin while helper discovery is still pending.
    let (mut peer, _) = listener.accept().await.unwrap();
    assert!(matches!(
      ctl_ipc::read_frame::<_, ctl_ipc::lifecycle::Request>(&mut peer)
        .await
        .unwrap(),
      Some(ctl_ipc::lifecycle::Request::CtldInspect { .. })
    ));
    ctl_ipc::write_frame(
      &mut peer,
      &ctl_ipc::lifecycle::Response::CtldInfo {
        protocol_version: ctl_ipc::lifecycle::PROTOCOL_VERSION,
        info: ctl_ipc::lifecycle::DaemonInfo {
          instance_id: "existing-owner".into(),
          binary: crate::daemon_helper::tests::binary_info(),
          active_vpn_count: 0,
        },
      },
    )
    .await
    .unwrap();
    drop(peer);
    tokio::time::pause();
    tokio::time::advance(crate::daemon_helper::PREPARATION_TIMEOUT).await;
    let row = row.await.unwrap();
    assert!(row.running.is_some(), "{row:?}");
    assert!(row.available.is_none(), "{row:?}");
    assert!(!row.restart_supported);
    assert!(
      row
        .error
        .unwrap()
        .contains(crate::daemon_helper::TIMEOUT_MESSAGE)
    );
  }

  fn owner(socket: &str, label: &str) -> Owner {
    Owner {
      id: String::new(),
      label: label.into(),
      socket: socket.into(),
      executable: Ok("helper".into()),
    }
  }

  #[test]
  fn selected_ssh_and_vpn_owners_share_one_row_only_for_the_same_endpoint() {
    let shared = deduplicate_owners(
      owner("/tmp/about-shared.sock", "ctld (SSH)"),
      owner("/tmp/about-shared.sock", "ctld (VPN)"),
    );
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0].label, "ctld (SSH, VPN)");
    assert!(!shared[0].id.contains("about-shared"));
    let split = deduplicate_owners(
      owner("/tmp/about-ssh.sock", "ctld (SSH)"),
      owner("/tmp/about-vpn.sock", "ctld (VPN)"),
    );
    assert_eq!(split.len(), 2);
    assert_eq!(split[0].label, "ctld (SSH)");
    assert_eq!(split[1].label, "ctld (VPN)");
    assert_ne!(split[0].id, split[1].id);
  }

  #[tokio::test]
  async fn unsupported_helper_query_cannot_launch_a_daemon() {
    let error = read_binary(
      PathBuf::from("/nonexistent/ctmux-about-test-helper"),
      "ctmuxd",
    )
    .await
    .unwrap_err();
    assert_ne!(error, String::new());
  }

  #[test]
  fn legacy_versions_are_only_accepted_for_the_expected_component() {
    assert_eq!(
      legacy_binary_version(b"ctmuxd 0.1.0\n", "ctmuxd")
        .unwrap()
        .version
        .as_deref(),
      Some("0.1.0")
    );
    for output in [
      b"ctld 0.1.0".as_slice(),
      b"ctmuxd invalid",
      b"ctmuxd 0.1.0 trailing",
    ] {
      assert!(legacy_binary_version(output, "ctmuxd").is_err());
    }
  }

  #[test]
  fn structured_versions_require_the_correct_component_but_preserve_protocol_mismatches() {
    let mut info = ctl_core::component::ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: vec![ctl_core::component::ProtocolInfo::new(
        "ctmux",
        ctmux_proto::PROTOCOL_BUILD + 1,
        ctl_core::protocol::ProtocolVersion::new(1, 0, ctmux_proto::PROTOCOL_BUILD + 1),
        &[ctl_core::protocol::ProtocolVersion::new(
          1,
          0,
          ctmux_proto::PROTOCOL_BUILD + 1,
        )],
      )],
    };
    let encoded = serde_json::to_vec(&info).unwrap();
    assert!(parse_binary_info(&encoded, "ctld").is_err());
    assert!(parse_binary_info(&encoded, "ctl-taskd").is_err());
    assert_eq!(
      parse_binary_info(&encoded, "ctmuxd").unwrap().protocols[0].version,
      ctl_core::protocol::ProtocolVersion::new(1, 0, ctmux_proto::PROTOCOL_BUILD + 1)
    );
    info.protocols.push(info.protocols[0].clone());
    assert!(parse_binary_info(&serde_json::to_vec(&info).unwrap(), "ctmuxd").is_err());
    info.protocols.pop();
    info.build.source_fingerprint = "malformed".into();
    assert!(parse_binary_info(&serde_json::to_vec(&info).unwrap(), "ctmuxd").is_err());
  }

  #[test]
  fn restart_actions_require_all_replacement_protocols_and_build_metadata() {
    for component in ["ctmuxd", "ctl-taskd"] {
      let mut row = ComponentVersionRow::local(component, component);
      let mut info = ComponentVersionInfo::from_build(
        ctl_core::component::build_info(),
        row.required_protocols.clone(),
      );
      row.available = Some(info.clone());
      assert!(compatible_replacement(&row));
      info.protocols.pop();
      row.available = Some(info.clone());
      assert!(!compatible_replacement(&row));
      info.protocols.clone_from(&row.required_protocols);
      info.source_fingerprint = None;
      row.available = Some(info);
      assert!(!compatible_replacement(&row));
    }
  }
}
