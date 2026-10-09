use super::{Daemon, build_label, compatible, output, remote, validate_target};
use ctl_core::component::ComponentInfo;
use std::io;
use std::io::IsTerminal as _;

fn restart_error(code: &str, error: &impl std::fmt::Display, may_have_stopped: bool) -> io::Error {
  let guidance = if may_have_stopped {
    "; the owner may have stopped. Run components status before retrying"
  } else {
    "; no service was changed"
  };
  io::Error::other(format!("{code}: {error}{guidance}"))
}

// Holding the prepared request pins the owner and replacement across confirmation.
enum Prepared {
  Ctld(ctl_ipc::lifecycle::PreparedRestart),
  Ctmuxd(ctmux_ipc::lifecycle::PreparedRestart),
  Taskd(ctl_task_client::PreparedRestart),
  Remote(ctl_client::maintenance::PreparedRemoteCtmuxRestart),
}
impl Prepared {
  fn before(&self) -> serde_json::Value {
    match self {
      Self::Ctld(prepared) => serde_json::json!(prepared.before),
      Self::Ctmuxd(prepared) => serde_json::json!(prepared.before.component_info()),
      Self::Taskd(_) => serde_json::Value::Null,
      Self::Remote(prepared) => serde_json::json!(prepared.info.running),
    }
  }

  fn available(&self) -> ComponentInfo {
    match self {
      Self::Ctld(prepared) => ComponentInfo {
        build: prepared.available.info.build.clone(),
        protocols: prepared.available.info.protocols.clone(),
      },
      Self::Ctmuxd(prepared) => prepared.available.clone(),
      Self::Taskd(prepared) => prepared.available.clone(),
      Self::Remote(prepared) => prepared.info.available.clone(),
    }
  }
  async fn restart(self) -> io::Result<serde_json::Value> {
    match self {
      Self::Ctld(prepared) => {
        let outcome = prepared
          .restart()
          .await
          .map_err(|error| restart_error(error.code(), &error, true))?;
        Ok(serde_json::json!({ "after": outcome.after }))
      }
      Self::Ctmuxd(prepared) => {
        let outcome = prepared
          .restart()
          .await
          .map_err(|error| restart_error(error.code(), &error, error.may_have_stopped()))?;
        Ok(
          serde_json::json!({ "after": outcome.after, "terminated_sessions": outcome.terminated_sessions }),
        )
      }
      Self::Taskd(prepared) => {
        let outcome = prepared
          .restart()
          .await
          .map_err(|error| restart_error(error.code(), &error, error.may_have_stopped()))?;
        Ok(serde_json::json!({ "after": outcome.after }))
      }
      Self::Remote(prepared) => {
        let outcome = prepared
          .restart()
          .await
          .map_err(|error| restart_error(&error.code, &error, error.may_have_stopped))?;
        Ok(
          serde_json::json!({ "after": outcome.after, "terminated_sessions": outcome.terminated_sessions }),
        )
      }
    }
  }
}

async fn prepare_restart(
  component: Daemon,
  host: Option<&str>,
  method: Option<&str>,
) -> io::Result<Prepared> {
  if let Some(host) = host {
    let remote = remote(host, method).await?;
    let prepared = ctl_client::maintenance::prepare_ctmux_restart(
      &remote.destination,
      &remote.options,
      &remote.control_path,
      &remote.remote_id,
    )
    .await
    .map_err(io::Error::other)?;
    if !compatible(component, &prepared.info.available) {
      return Err(io::Error::other(
        "remote replacement is incompatible with this CLI",
      ));
    }
    Ok(Prepared::Remote(prepared))
  } else {
    Ok(match component {
      Daemon::Ctld => {
        let client = ctl_ipc::lifecycle::Client::new(ctl_ipc::socket_path());
        // Observe first so an absent owner does not prepare/install a helper.
        // Preflight then uses the registered async signed-helper provider and
        // pins the owner again; synchronous discovery would bypass that provider.
        if matches!(
          client.probe().await.map_err(io::Error::other)?,
          ctl_ipc::lifecycle::DaemonStatus::Absent
        ) {
          return Err(io::Error::other(
            "ctld is not running; restart does not start an absent owner",
          ));
        }
        let prepared = client.preflight_restart().await.map_err(io::Error::other)?;
        if prepared.before.is_none() {
          return Err(io::Error::other(
            "ctld is not running; restart does not start an absent owner",
          ));
        }
        Prepared::Ctld(prepared)
      }
      Daemon::Ctmuxd => Prepared::Ctmuxd(
        ctmux_ipc::lifecycle::Client::new(ctmux_ipc::socket_path())
          .with_daemon_executable(component.executable()?)
          .preflight_restart()
          .await
          .map_err(io::Error::other)?,
      ),
      Daemon::CtlTaskd => Prepared::Taskd(
        ctl_task_client::preflight_restart_at(ctl_task_ipc::socket_path(), component.executable()?)
          .await
          .map_err(io::Error::other)?,
      ),
    })
  }
}

pub(crate) async fn run(
  component: Daemon,
  host: Option<&str>,
  method: Option<&str>,
  platform: Option<crate::RemotePlatform>,
  yes: bool,
  dry_run: bool,
  json: bool,
) -> io::Result<()> {
  validate_target(host, method, platform, Some(component))?;
  if !yes && !dry_run && (!io::stdin().is_terminal() || !io::stderr().is_terminal()) {
    return Err(io::Error::other(
      "restart requires an interactive terminal or explicit --yes; use --dry-run to inspect the plan",
    ));
  }
  let prepared = prepare_restart(component, host, method).await?;
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
  let available = prepared.available();
  let plan = serde_json::json!({ "host": host.unwrap_or("local"), "component": component, "before": prepared.before(), "available": available, "impact": component.impact(), "dry_run": dry_run });
  eprintln!(
    "{} on {} → {}\n{}",
    component.name(),
    crate::table::text(host.unwrap_or("local")),
    build_label(Some(&available)),
    component.impact()
  );
  if dry_run {
    if json {
      output(&plan)?;
    } else {
      println!("Preflight passed. No service was changed.");
    }
    return Ok(());
  }
  if !yes {
    let approved = tokio::task::spawn_blocking(|| {
      cliclack::confirm("Restart this daemon? (Preparation expires after 20 seconds)")
        .initial_value(false)
        .interact()
    })
    .await
    .map_err(io::Error::other)??;
    if !approved {
      return Err(io::Error::new(
        io::ErrorKind::Interrupted,
        "Restart cancelled; no service was changed.",
      ));
    }
  }
  if tokio::time::Instant::now() >= deadline {
    return Err(io::Error::other(
      "Restart preparation expired; no service was changed. Run the command again.",
    ));
  }
  // Once confirmed, await the cooperative request and verified successor. Never
  // force-kill an owner or retry a mutation on an uncertain result.
  let outcome = prepared.restart().await?;
  if json {
    output(
      &serde_json::json!({ "host": host.unwrap_or("local"), "component": component, "result": outcome }),
    )?;
  } else {
    println!(
      "{} restarted and its replacement was verified.",
      component.name()
    );
    if let Some(count) = outcome.get("terminated_sessions") {
      println!("Terminated sessions: {count}");
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::{Daemon, prepare_restart};
  use std::io;
  use std::sync::atomic::{AtomicUsize, Ordering};

  static PROVIDER_CALLS: AtomicUsize = AtomicUsize::new(0);
  const PROVIDER_ERROR: &str = "maintenance fixture signed-helper provider consulted";

  #[tokio::test]
  async fn ctld_restart_uses_provider_preserves_override_and_skips_preparation_when_absent() {
    let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    for mode in ["provider", "override", "absent"] {
      let directory = std::env::temp_dir().join(format!(
        "cr-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
      ));
      std::fs::create_dir(&directory).unwrap();
      let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
      command
        .args([
          "--exact",
          "components::maintenance::restart::tests::provider_child",
          "--nocapture",
        ])
        .env("CTL_MAINTENANCE_TEST_MODE", mode)
        .env("CTLD_SOCKET_PATH", directory.join("ctld.sock"))
        .env("HOME", &directory)
        .env("PATH", &directory)
        .env_remove("CTLD_BIN")
        .kill_on_drop(true);
      if mode == "override" {
        command.env("CTLD_BIN", directory.join("explicit-missing-ctld"));
      }
      let output = tokio::time::timeout(std::time::Duration::from_secs(15), command.output())
        .await
        .unwrap()
        .unwrap();
      std::fs::remove_dir_all(directory).unwrap();
      assert!(output.status.success(), "{mode}: {output:?}");
    }
  }

  #[tokio::test]
  async fn provider_child() {
    let Ok(mode) = std::env::var("CTL_MAINTENANCE_TEST_MODE") else {
      return;
    };
    // The registry is process-global. Each scenario runs in an isolated child,
    // with a provider failure sentinel rather than executable/signing fixtures.
    ctl_ipc::register_preferred_contract_daemon_executable_provider(|_| {
      Box::pin(async {
        PROVIDER_CALLS.fetch_add(1, Ordering::SeqCst);
        Err(io::Error::other(PROVIDER_ERROR))
      })
    })
    .unwrap();
    let socket = ctl_ipc::socket_path();
    let server = if mode == "absent" {
      None
    } else {
      let listener = tokio::net::UnixListener::bind(&socket).unwrap();
      Some(tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(matches!(
          ctl_ipc::read_frame::<_, ctl_ipc::lifecycle::Request>(&mut stream)
            .await
            .unwrap(),
          Some(ctl_ipc::lifecycle::Request::CtldInspect { .. })
        ));
        ctl_ipc::write_frame(
          &mut stream,
          &ctl_ipc::lifecycle::Response::CtldInfo {
            protocol_version: ctl_ipc::lifecycle::PROTOCOL_VERSION,
            info: ctl_ipc::lifecycle::DaemonInfo {
              instance_id: uuid::Uuid::new_v4().to_string(),
              binary: ctl_ipc::lifecycle::DaemonBinaryInfo::current(),
              active_vpn_count: 0,
            },
          },
        )
        .await
        .unwrap();
        assert!(
          ctl_ipc::read_frame::<_, ctl_ipc::lifecycle::Request>(&mut stream)
            .await
            .unwrap()
            .is_none()
        );
      }))
    };
    let error = prepare_restart(Daemon::Ctld, None, None)
      .await
      .err()
      .expect("fixture must stop before mutation");
    match mode.as_str() {
      "provider" => {
        assert!(error.to_string().contains(PROVIDER_ERROR), "{error}");
        assert_eq!(PROVIDER_CALLS.load(Ordering::SeqCst), 1);
      }
      "override" => {
        assert!(!error.to_string().contains(PROVIDER_ERROR), "{error}");
        assert_eq!(PROVIDER_CALLS.load(Ordering::SeqCst), 0);
      }
      "absent" => {
        assert!(error.to_string().contains("ctld is not running"), "{error}");
        assert_eq!(PROVIDER_CALLS.load(Ordering::SeqCst), 0);
        assert!(!socket.exists());
      }
      _ => panic!("unknown fixture mode"),
    }
    if let Some(server) = server {
      tokio::time::timeout(std::time::Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
    }
  }
}
