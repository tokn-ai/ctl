use super::{Daemon, confirm, output, remote, require_approval, validate_target};
use crate::components::TargetOptions;
use std::io;

enum Prepared {
  Ctld(ctl_ipc::lifecycle::PreparedStop),
  Ctmuxd(ctmux_ipc::lifecycle::PreparedStop),
  Taskd(ctl_task_client::restart::PreparedStop),
  Remote(ctl_client::maintenance::PreparedRemoteCtmuxStop),
}
impl Prepared {
  async fn stop(self) -> io::Result<serde_json::Value> {
    match self {
      Self::Ctld(prepared) => {
        let before = prepared.stop().await.map_err(|error| {
          let uncertain = !matches!(&error, ctl_ipc::lifecycle::LifecycleError::Rejected { code, .. } if code == "ctld_stop_expired");
          failed(&error, uncertain)
        })?;
        Ok(serde_json::json!({ "before": before }))
      }
      Self::Ctmuxd(prepared) => {
        let count = prepared
          .stop()
          .await
          .map_err(|error| failed(&error, error.may_have_stopped()))?;
        Ok(serde_json::json!({ "terminated_sessions": count }))
      }
      Self::Taskd(prepared) => {
        prepared
          .stop()
          .await
          .map_err(|error| failed(&error, error.may_have_stopped()))?;
        Ok(serde_json::json!({}))
      }
      Self::Remote(prepared) => {
        let outcome = prepared
          .stop()
          .await
          .map_err(|error| failed(&error, error.may_have_stopped))?;
        Ok(serde_json::json!({ "terminated_sessions": outcome.terminated_sessions }))
      }
    }
  }
}
fn failed(error: &impl std::fmt::Display, uncertain: bool) -> io::Error {
  let guidance = if uncertain {
    "the owner may have stopped; check components status before retrying"
  } else {
    "no service was changed"
  };
  io::Error::other(format!("{error}; {guidance}"))
}
async fn prepare(component: Daemon, target: TargetOptions<'_>) -> io::Result<Option<Prepared>> {
  if target.host.is_some() || target.destination.is_some() {
    let remote = remote(target).await?;
    return ctl_client::maintenance::prepare_ctmux_stop(
      &remote.destination,
      &remote.options,
      &remote.control_path,
      &remote.remote_id,
    )
    .await
    .map(|prepared| Some(Prepared::Remote(prepared)))
    .map_err(io::Error::other);
  }
  Ok(match component {
    Daemon::Ctld => {
      let client = ctl_ipc::lifecycle::Client::new(ctl_ipc::socket_path());
      if matches!(
        client.probe().await.map_err(io::Error::other)?,
        ctl_ipc::lifecycle::DaemonStatus::Absent
      ) {
        None
      } else {
        Some(Prepared::Ctld(
          client.preflight_stop().await.map_err(io::Error::other)?,
        ))
      }
    }
    Daemon::Ctmuxd => {
      let client = ctmux_ipc::lifecycle::Client::new(ctmux_ipc::socket_path());
      if client.observe().await.map_err(io::Error::other)?.is_none() {
        None
      } else {
        Some(Prepared::Ctmuxd(
          client.preflight_stop().await.map_err(io::Error::other)?,
        ))
      }
    }
    Daemon::CtlTaskd => {
      if ctl_task_ipc::component_status().await?.is_none() {
        None
      } else {
        Some(Prepared::Taskd(
          ctl_task_client::restart::Client::new(ctl_task_ipc::socket_path())
            .preflight_stop()
            .await
            .map_err(io::Error::other)?,
        ))
      }
    }
  })
}

pub(crate) async fn run(
  component: Daemon,
  target: TargetOptions<'_>,
  yes: bool,
  dry_run: bool,
  json: bool,
) -> io::Result<()> {
  validate_target(target, Some(component))?;
  require_approval("stop", yes, dry_run)?;
  let label = target.host.or(target.destination).unwrap_or("local");
  let Some(prepared) = prepare(component, target).await? else {
    if json {
      output(
        &serde_json::json!({ "host": label, "component": component, "already_stopped": true, "dry_run": dry_run }),
      )?;
    } else {
      println!("{} is already stopped.", component.name());
    }
    return Ok(());
  };
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
  let impact = match component {
    Daemon::Ctld => {
      "Disconnects this ctld's clients and stops its VPNs; saved hosts and credentials are preserved."
    }
    Daemon::Ctmuxd => {
      "Ends ALL sessions and panes owned by this ctmuxd endpoint, including other clients and interactive tasks. Session contents cannot be recovered."
    }
    Daemon::CtlTaskd => {
      "Stops only an idle task owner. Active tasks cause refusal; definitions, history and terminal sessions are preserved."
    }
  };
  eprintln!(
    "Stop {} on {}\n{impact}\nNo replacement daemon will be launched.",
    component.name(),
    crate::table::text(label)
  );
  if dry_run {
    if json {
      output(
        &serde_json::json!({ "host": label, "component": component, "impact": impact, "dry_run": true }),
      )?;
    } else {
      println!("Preflight passed. No service was changed.");
    }
    return Ok(());
  }
  confirm("Stop", yes, deadline).await?;
  let result = prepared.stop().await?;
  if json {
    output(&serde_json::json!({ "host": label, "component": component, "result": result }))?;
  } else {
    println!("{} stopped; no replacement was launched.", component.name());
  }
  Ok(())
}
