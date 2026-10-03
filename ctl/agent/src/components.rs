//! Passive inspection of fixed companions and account-owned service endpoints.
use std::{io, path::Path, time::Duration};

use ctl_core::component::{ComponentInfo, LegacyProtocolInfo};
use ctl_proto::maintenance::{
  self, ClientMessage, ComponentKind as Kind, ComponentState as State, RemoteComponent,
  RemoteComponents, ServerMessage,
};
use tokio::io::{AsyncRead, AsyncWrite};

/// Handles one identity-bound, read-only component inspection request.
///
/// # Errors
/// Returns bounded transport errors; request errors receive structured replies.
pub async fn serve<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
  reader: &mut R,
  writer: &mut W,
  directory: &Path,
  remote_id: &str,
) -> io::Result<()> {
  let request = tokio::time::timeout(Duration::from_secs(5), maintenance::read(reader)).await;
  let response = match request {
    Ok(Ok(ClientMessage::InspectComponents {
      protocol,
      expected_remote_id,
    }))
      if !expected_remote_id.is_empty()
        && expected_remote_id == remote_id
        && protocol
          .negotiate(&[maintenance::CONTRACT_V1_0_3])
          .is_some() =>
    {
      ServerMessage::Components {
        protocol_version: maintenance::CONTRACT_V1_0_3,
        snapshot: inspect(directory, remote_id).await,
      }
    }
    _ => ServerMessage::Error {
      code: "remote_inspection_unavailable".into(),
      message: "Remote identity or component inspection protocol changed.".into(),
      may_have_stopped: false,
    },
  };
  maintenance::write(writer, &response).await
}

async fn installed(directory: &Path, name: &str) -> Result<ComponentInfo, String> {
  ctl_core::executable::inspect(&directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
    .await
    .map_err(|error| error.to_string())
}

fn row(component: Kind, installed: Result<ComponentInfo, String>) -> RemoteComponent {
  let (installed, error) = match installed {
    Ok(info) => (Some(info), None),
    Err(error) => (None, Some(error)),
  };
  RemoteComponent {
    component,
    installed,
    running: None,
    state: State::Unavailable,
    restart_supported: false,
    legacy_protocols: Vec::new(),
    error,
  }
}

fn failed(row: &mut RemoteComponent, error: impl std::fmt::Display) {
  row.state = State::Unavailable;
  row.error = Some(match row.error.take() {
    Some(previous) => format!("{previous}; {error}"),
    None => error.to_string(),
  });
}

async fn inspect(directory: &Path, remote_id: &str) -> RemoteComponents {
  let broker_client = ctl_ipc::lifecycle::Client::new(ctl_ipc::default_socket_path());
  let terminal_client = ctmux_ipc::lifecycle::Client::new(ctmux_ipc::socket_path());
  let (ctld_installed, ctmux_installed, task_installed, ctld, ctmux, task) = tokio::join!(
    installed(directory, "ctld"),
    installed(directory, "ctmuxd"),
    installed(directory, "ctl-taskd"),
    broker_client.probe(),
    terminal_client.observe(),
    ctl_task_ipc::component_status(),
  );
  let mut agent = row(Kind::CtlAgent, Ok(crate::component_info()));
  // This query is a disposable agent process, not a running terminal transport.
  agent.state = State::OnDemand;
  let mut broker = row(Kind::Ctld, ctld_installed);
  match ctld {
    Ok(ctl_ipc::lifecycle::DaemonStatus::Absent) => broker.state = State::NotRunning,
    Ok(ctl_ipc::lifecycle::DaemonStatus::Legacy { protocol_version }) => {
      broker.state = State::Legacy;
      broker.legacy_protocols = protocol_version
        .map(|version| LegacyProtocolInfo {
          name: "ctld".into(),
          version,
        })
        .into_iter()
        .collect();
    }
    Ok(ctl_ipc::lifecycle::DaemonStatus::Running { info }) => {
      broker.state = State::Running;
      broker.running = Some(ComponentInfo {
        build: info.binary.build,
        protocols: info.binary.protocols,
      });
    }
    Err(error) => failed(&mut broker, error),
  }
  let mut terminal = row(Kind::Ctmuxd, ctmux_installed);
  match ctmux {
    Ok(None) => terminal.state = State::NotRunning,
    Ok(Some(info)) => {
      terminal.state = if info.legacy_protocols.is_empty() {
        State::Running
      } else {
        State::Legacy
      };
      terminal.restart_supported = info.restart_supported;
      terminal.legacy_protocols = info.legacy_protocols;
      terminal.running = info.build.map(|build| ComponentInfo {
        build,
        protocols: info.protocols,
      });
    }
    Err(error) => failed(&mut terminal, error),
  }
  let mut tasks = row(Kind::CtlTaskd, task_installed);
  match task {
    Ok(None) => tasks.state = State::NotRunning,
    Ok(Some(info)) => {
      tasks.state = State::Running;
      tasks.running = info.build.map(|build| ComponentInfo {
        build,
        protocols: info.protocols,
      });
      if info.protocol_mismatch {
        tasks.error =
          Some("The running task daemon does not support this agent's protocols.".into());
      }
    }
    Err(error) => failed(&mut tasks, error),
  }
  RemoteComponents {
    remote_id: remote_id.into(),
    components: vec![agent, broker, terminal, tasks],
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(unix)]
  #[tokio::test]
  async fn inspection_reports_installed_companions_without_starting_absent_services() {
    use std::os::unix::fs::PermissionsExt as _;
    let root =
      std::path::PathBuf::from("/tmp").join(format!("ctl-inspection-{}", uuid::Uuid::new_v4()));
    let directory = root.join("bundle");
    std::fs::create_dir_all(&directory).unwrap();
    let info = crate::component_info();
    for name in ["ctld", "ctmuxd", "ctl-taskd"] {
      let path = directory.join(name);
      std::fs::write(&path, format!("#!/bin/sh\nif [ \"$1\" = --component-info ]; then\nprintf '%s\\n' '{}'\nelse\nprintf started > '{}'\nfi\n", serde_json::to_string(&info).unwrap(), root.join("started").display())).unwrap();
      std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let status = tokio::process::Command::new(std::env::current_exe().unwrap())
      .args(["--exact", "components::tests::inspection_child"])
      .env("CTL_INSPECTION_TEST_ROOT", &root)
      .env("XDG_RUNTIME_DIR", root.join("runtime"))
      .env("CTMUX_RUNTIME_DIR", root.join("runtime/ctmux"))
      .env("CTL_TASKD_RUNTIME_DIR", root.join("runtime/task"))
      .kill_on_drop(true)
      .status()
      .await
      .unwrap();
    assert!(status.success());
    assert!(!root.join("started").exists());
    assert!(!root.join("runtime").exists());
    std::fs::remove_dir_all(root).unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn inspection_child() {
    let Some(root) = std::env::var_os("CTL_INSPECTION_TEST_ROOT") else {
      return;
    };
    let mut request = Vec::new();
    maintenance::write(
      &mut request,
      &ClientMessage::InspectComponents {
        protocol: maintenance::protocol_offer(),
        expected_remote_id: "pinned".into(),
      },
    )
    .await
    .unwrap();
    let mut output = Vec::new();
    serve(
      &mut request.as_slice(),
      &mut output,
      &std::path::PathBuf::from(root).join("bundle"),
      "pinned",
    )
    .await
    .unwrap();
    let ServerMessage::Components {
      protocol_version,
      snapshot,
    } = maintenance::read(&mut output.as_slice()).await.unwrap()
    else {
      panic!("Expected component inventory")
    };
    assert_eq!(protocol_version, maintenance::CONTRACT_V1_0_3);
    assert_eq!(snapshot.remote_id, "pinned");
    assert_eq!(snapshot.components.len(), 4);
    for row in snapshot.components {
      assert!(row.installed.is_some());
      assert!(row.running.is_none());
      assert!(!row.restart_supported);
      assert_eq!(
        row.state,
        if row.component == Kind::CtlAgent {
          State::OnDemand
        } else {
          State::NotRunning
        }
      );
    }
  }

  #[tokio::test]
  async fn wrong_identity_and_old_contract_do_not_inspect_or_start_services() {
    for (remote_id, protocol) in [
      ("other", maintenance::protocol_offer()),
      (
        "pinned",
        ctl_core::protocol::ProtocolOffer::new(
          2,
          maintenance::CONTRACT_V1_0_2,
          &[maintenance::CONTRACT_V1_0_2],
        ),
      ),
    ] {
      let mut input = Vec::new();
      maintenance::write(
        &mut input,
        &ClientMessage::InspectComponents {
          protocol,
          expected_remote_id: remote_id.into(),
        },
      )
      .await
      .unwrap();
      let mut output = Vec::new();
      serve(
        &mut input.as_slice(),
        &mut output,
        Path::new("/not-an-installed-bundle"),
        "pinned",
      )
      .await
      .unwrap();
      assert!(matches!(
        maintenance::read::<_, ServerMessage>(&mut output.as_slice())
          .await
          .unwrap(),
        ServerMessage::Error {
          may_have_stopped: false,
          ..
        }
      ));
    }
  }
}
