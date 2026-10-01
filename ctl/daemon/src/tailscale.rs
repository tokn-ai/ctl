//! Persistent Tailscale identities with a short-lived, ctld-owned container.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use ctld_ipc::{VpnConnection, VpnProvider, VpnSettings, VpnState, VpnStatus};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Child;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep, timeout};

use crate::openconnect::{engine_command, find_engine, parse_published_port};

mod socks;
use socks::ready as socks_ready;

const IMAGE: &str = "docker.io/tailscale/tailscale:v1.94.2";
const ENTRYPOINT: &str = include_str!("../../../docker/tailscale/entrypoint.sh");
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const START_TIMEOUT: Duration = Duration::from_mins(2);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

pub(super) struct Config {
  container_name: String,
  hostname: String,
  accept_routes: bool,
  pub(super) cancellation: Option<oneshot::Receiver<()>>,
}

impl Config {
  pub(super) fn from_connection(connection: &VpnConnection) -> io::Result<Self> {
    connection
      .validate()
      .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let VpnSettings::Tailscale {
      hostname,
      accept_routes,
    } = &connection.settings
    else {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "expected a Tailscale profile",
      ));
    };
    let home =
      dirs::home_dir().ok_or_else(|| io::Error::other("could not locate VPN state owner"))?;
    let container_name = container_name(&home, &connection.connection_id);
    Ok(Self {
      hostname: hostname
        .clone()
        .unwrap_or_else(|| format!("rmux-{}", &container_name[15..27])),
      container_name,
      accept_routes: *accept_routes,
      cancellation: None,
    })
  }
}

fn container_name(owner: &Path, connection_id: &str) -> String {
  let mut digest = Sha256::new();
  digest.update(owner.as_os_str().as_encoded_bytes());
  digest.update([0]);
  digest.update(connection_id.as_bytes());
  format!("ctld-tailscale-{:x}", digest.finalize())
}

pub(super) struct ManagedVpn {
  child: Child,
  heartbeat: JoinHandle<()>,
  monitor: Option<JoinHandle<()>>,
  status: watch::Receiver<VpnStatus>,
  engine: PathBuf,
  container_name: String,
  container_id: Option<String>,
  lease_id: String,
}

impl ManagedVpn {
  pub(super) fn status(&self) -> VpnStatus {
    self.status.borrow().clone()
  }

  pub(super) async fn exited(&mut self) -> io::Result<ExitStatus> {
    self.child.wait().await
  }

  pub(super) async fn shutdown(&mut self) {
    self.heartbeat.abort();
    if let Some(monitor) = self.monitor.take() {
      monitor.abort();
    }
    if let Some(container_id) = &self.container_id {
      let _ = timeout(
        COMMAND_TIMEOUT,
        engine_command(&self.engine)
          .args(["stop", "--time", "3", container_id])
          .stdout(Stdio::null())
          .status(),
      )
      .await;
      // An engine may report a successful stop for a Created container without
      // applying --rm. Remove it explicitly by the verified immutable ID too.
      let _ = timeout(
        COMMAND_TIMEOUT,
        engine_command(&self.engine)
          .args(["rm", "--force", container_id])
          .stdout(Stdio::null())
          .status(),
      )
      .await;
    }
    let _ = self.child.start_kill();
    let _ = timeout(COMMAND_TIMEOUT, self.child.wait()).await;
  }

  async fn cancel_startup(&mut self) {
    self.heartbeat.abort();
    let _ = self.child.start_kill();
    let _ = timeout(COMMAND_TIMEOUT, self.child.wait()).await;
    // Create may have reached the engine just before the attached client died.
    // Inspect after reaping it, including a short window for a late engine
    // response. Created-but-not-started containers have no heartbeat watchdog.
    // Never remove a reservation with another owner's random lease label.
    let _ = timeout(Duration::from_secs(2), async {
      loop {
        match inspect_owned(&self.engine, &self.container_name, &self.lease_id).await {
          Ok(container) => {
            self.container_id = Some(container.id);
            break;
          }
          Err(error) if error.kind() == io::ErrorKind::PermissionDenied => break,
          Err(_) => sleep(Duration::from_millis(50)).await,
        }
      }
    })
    .await;
    self.shutdown().await;
  }

  fn start_monitor(&mut self, updates: watch::Sender<VpnStatus>, port: u16) {
    let engine = self.engine.clone();
    let container_name = self.container_name.clone();
    let container_id = self
      .container_id
      .clone()
      .expect("ready container has an ID");
    self.monitor = Some(tokio::spawn(async move {
      let mut ticks = interval(Duration::from_secs(2));
      ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
      loop {
        ticks.tick().await;
        let status = if let Ok(backend) = backend_status(&engine, &container_id).await {
          let status = backend.status(&container_name, port);
          if status.running && socks_ready(port).await.is_err() {
            let mut pending = pending_status(&container_name);
            pending.message = Some("Waiting for the Tailscale SOCKS5 proxy".into());
            pending
          } else {
            status
          }
        } else {
          let mut status = pending_status(&container_name);
          status.message = Some("Waiting for the Tailscale service".into());
          status
        };
        if updates.send(status).is_err() {
          break;
        }
      }
    }));
  }

  async fn wait_ready(&mut self) -> io::Result<(u16, BackendStatus)> {
    timeout(START_TIMEOUT, async {
      loop {
        if self.child.try_wait()?.is_some() {
          return Err(io::Error::other("Tailscale container exited; check the container engine and image availability, or whether this profile is already owned by another ctld"));
        }
        if let Ok((container_id, port)) = published_port(&self.engine, &self.container_name, &self.lease_id).await
          && let Ok(status) = backend_status(&self.engine, &container_id).await
          && status.is_actionable()
          && socks_ready(port).await.is_ok()
        {
          self.container_id = Some(container_id);
          return Ok((port, status));
        }
        sleep(Duration::from_millis(250)).await;
      }
    }).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Tailscale container did not start within 120 seconds; check the container engine and image download"))?
  }
}

impl Drop for ManagedVpn {
  fn drop(&mut self) {
    self.heartbeat.abort();
    if let Some(monitor) = &self.monitor {
      monitor.abort();
    }
    let _ = self.child.start_kill();
  }
}

pub(super) async fn start(config: Config) -> io::Result<ManagedVpn> {
  start_config(config, &find_engine()?).await
}

pub(super) async fn forget_identity(connection_id: &str) -> io::Result<()> {
  forget_identity_with_engine(connection_id, &find_engine()?).await
}

async fn forget_identity_with_engine(connection_id: &str, engine: &Path) -> io::Result<()> {
  let config = Config::from_connection(&VpnConnection {
    connection_id: connection_id.into(),
    name: "Tailscale enrollment".into(),
    settings: VpnSettings::Tailscale {
      hostname: None,
      accept_routes: false,
    },
  })?;
  let containers = timeout(
    COMMAND_TIMEOUT,
    engine_command(engine)
      .args([
        "container",
        "ls",
        "--all",
        "--quiet",
        "--filter",
        &format!("name={}", config.container_name),
      ])
      .output(),
  )
  .await??;
  if !containers.status.success() {
    return Err(io::Error::other(
      "Could not check Tailscale identity ownership; start the container engine and try again",
    ));
  }
  if !containers.stdout.iter().all(u8::is_ascii_whitespace) {
    return Err(io::Error::other(
      "Tailscale identity is still owned by a container; stop it before forgetting the identity",
    ));
  }
  let volume = format!("{}-state", config.container_name);
  let volumes = timeout(
    COMMAND_TIMEOUT,
    engine_command(engine)
      .args([
        "volume",
        "ls",
        "--quiet",
        "--filter",
        &format!("name={volume}"),
      ])
      .output(),
  )
  .await??;
  if !volumes.status.success() {
    return Err(io::Error::other(
      "Could not check Tailscale identity storage; start the container engine and try again",
    ));
  }
  if !volumes
    .stdout
    .split(|byte| *byte == b'\n')
    .any(|name| name == volume.as_bytes())
  {
    return Ok(());
  }
  // No --force: another owner may mount this identity after the name check.
  // The engine must reject removal while any container references the volume.
  let removed = timeout(
    COMMAND_TIMEOUT,
    engine_command(engine)
      .args(["volume", "rm", &volume])
      .stdout(Stdio::null())
      .status(),
  )
  .await??;
  if !removed.success() {
    return Err(io::Error::other(
      "Could not forget Tailscale identity; its state may still be in use by another container",
    ));
  }
  Ok(())
}

async fn cancelled(cancellation: Option<oneshot::Receiver<()>>) {
  if let Some(cancellation) = cancellation {
    let _ = cancellation.await;
  } else {
    std::future::pending::<()>().await;
  }
}

async fn start_config(config: Config, engine: &Path) -> io::Result<ManagedVpn> {
  // Docker/Podman reserve names atomically. The deterministic name prevents
  // another daemon mounting this profile's identity while its owner is alive.
  // A killed owner releases that reservation after the 15-second watchdog.
  let container_name = config.container_name;
  let cancellation = config.cancellation;
  let lease_id = uuid::Uuid::new_v4().simple().to_string();
  let volume = format!("{container_name}-state:/state");
  let mut child = engine_command(engine)
    .args([
      "run",
      "--rm",
      "--init",
      "--interactive",
      "--pull=missing",
      "--restart=no",
      "--name",
      &container_name,
      "--label",
      "io.ctl.service=tailscale",
      "--label",
      &format!("io.ctl.lease={lease_id}"),
      "--publish",
      "127.0.0.1::1080/tcp",
      "--volume",
      &volume,
      "--env",
      &format!("CTLD_HOSTNAME={}", config.hostname),
      "--env",
      &format!("CTLD_ACCEPT_ROUTES={}", config.accept_routes),
      "--entrypoint",
      "/bin/sh",
      IMAGE,
      "-c",
      ENTRYPOINT,
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .spawn()?;
  let mut stdin = child.stdin.take().expect("container stdin was piped");
  let heartbeat = tokio::spawn(async move {
    let mut ticks = interval(HEARTBEAT_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
      ticks.tick().await;
      if !matches!(
        timeout(HEARTBEAT_INTERVAL, stdin.write_all(b"ping\n")).await,
        Ok(Ok(()))
      ) {
        break;
      }
    }
  });
  let (updates, status) = watch::channel(pending_status(&container_name));
  let mut vpn = ManagedVpn {
    child,
    heartbeat,
    monitor: None,
    status,
    engine: engine.to_path_buf(),
    container_name,
    container_id: None,
    lease_id,
  };
  let startup = tokio::select! {
    result = vpn.wait_ready() => result,
    () = cancelled(cancellation) => {
      vpn.cancel_startup().await;
      return Err(io::Error::new(io::ErrorKind::Interrupted, "Tailscale startup was cancelled"));
    }
  };
  let (port, backend) = match startup {
    Ok(ready) => ready,
    Err(error) => {
      vpn.cancel_startup().await;
      return Err(error);
    }
  };
  updates.send_replace(backend.status(&vpn.container_name, port));
  vpn.start_monitor(updates, port);
  Ok(vpn)
}

async fn published_port(
  engine: &Path,
  container_name: &str,
  lease_id: &str,
) -> io::Result<(String, u16)> {
  let container = inspect_owned(engine, container_name, lease_id).await?;
  let network = container
    .network
    .ok_or_else(|| io::Error::other("container network is not available yet"))?;
  Ok((
    container.id,
    parse_published_port(&serde_json::to_vec(&network.ports)?)?,
  ))
}

#[derive(Deserialize)]
struct Container {
  #[serde(rename = "Id", alias = "ID")]
  id: String,
  #[serde(rename = "Config")]
  config: ContainerConfig,
  #[serde(rename = "NetworkSettings", default)]
  network: Option<ContainerNetwork>,
}

#[derive(Deserialize)]
struct ContainerConfig {
  #[serde(rename = "Labels", default)]
  labels: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct ContainerNetwork {
  #[serde(rename = "Ports", default)]
  ports: serde_json::Value,
}

async fn inspect_owned(
  engine: &Path,
  container_name: &str,
  lease_id: &str,
) -> io::Result<Container> {
  // Raw inspect JSON works across Docker and Podman. Their Go template data
  // exposes the container ID under incompatible field names (.Id versus .ID).
  let output = timeout(
    COMMAND_TIMEOUT,
    engine_command(engine)
      .args(["container", "inspect", container_name])
      .output(),
  )
  .await??;
  if !output.status.success() {
    return Err(io::Error::other("container port is not available yet"));
  }
  parse_owned_container(&output.stdout, lease_id)
}

fn parse_owned_container(bytes: &[u8], lease_id: &str) -> io::Result<Container> {
  let mut containers: Vec<Container> = serde_json::from_slice(bytes)?;
  if containers.len() != 1 {
    return Err(io::Error::other("expected one Tailscale container"));
  }
  let container = containers.pop().expect("exactly one inspected container");
  let lease = container
    .config
    .labels
    .as_ref()
    .and_then(|labels| labels.get("io.ctl.lease"));
  if lease.map(String::as_str) != Some(lease_id) || container.id.is_empty() {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "Tailscale container belongs to another owner",
    ));
  }
  Ok(container)
}

async fn backend_status(engine: &Path, container_name: &str) -> io::Result<BackendStatus> {
  let output = timeout(
    COMMAND_TIMEOUT,
    engine_command(engine)
      .args([
        "exec",
        container_name,
        "tailscale",
        "--socket=/run/tailscale/tailscaled.sock",
        "status",
        "--json",
        "--peers=false",
      ])
      .output(),
  )
  .await??;
  // `tailscale status` may exit nonzero before login while returning valid JSON.
  serde_json::from_slice(&output.stdout)
    .map_err(|_| io::Error::other("Tailscale status is not available yet"))
}

fn pending_status(container_name: &str) -> VpnStatus {
  VpnStatus {
    provider: VpnProvider::Tailscale,
    container_name: Some(container_name.into()),
    state: VpnState::Starting,
    ..VpnStatus::default()
  }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct BackendStatus {
  backend_state: String,
  #[serde(default, rename = "AuthURL")]
  auth_url: String,
  #[serde(rename = "Self")]
  local: Option<LocalStatus>,
  #[serde(default)]
  user: Option<HashMap<String, UserStatus>>,
  current_tailnet: Option<TailnetStatus>,
}

#[derive(Deserialize)]
struct LocalStatus {
  #[serde(rename = "UserID")]
  user_id: Option<u64>,
  #[serde(rename = "HostName")]
  hostname: Option<String>,
}

#[derive(Deserialize)]
struct TailnetStatus {
  #[serde(rename = "Name")]
  name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UserStatus {
  login_name: String,
}

impl BackendStatus {
  fn is_actionable(&self) -> bool {
    matches!(self.backend_state.as_str(), "Running" | "NeedsMachineAuth")
      || (self.backend_state == "NeedsLogin"
        && ctld_ipc::vpn::is_tailscale_auth_url(&self.auth_url))
  }

  fn status(&self, container_name: &str, port: u16) -> VpnStatus {
    let mut status = pending_status(container_name);
    match self.backend_state.as_str() {
      "Running" => {
        status.running = true;
        status.state = VpnState::Connected;
        status.endpoint = Some(format!("socks5h://127.0.0.1:{port}"));
        status.hostname = safe_name(
          self
            .local
            .as_ref()
            .and_then(|local| local.hostname.as_deref()),
        );
        status.tailnet = safe_name(
          self
            .current_tailnet
            .as_ref()
            .map(|tailnet| tailnet.name.as_str()),
        );
        status.username = self
          .local
          .as_ref()
          .and_then(|local| local.user_id)
          .and_then(|id| self.user.as_ref()?.get(&id.to_string()))
          .map(|user| &user.login_name)
          .filter(|name| {
            !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
          })
          .cloned();
      }
      "NeedsLogin" => {
        status.auth_url =
          ctld_ipc::vpn::is_tailscale_auth_url(&self.auth_url).then(|| self.auth_url.clone());
        status.message = Some(
          if status.auth_url.is_some() {
            "Sign in to Tailscale in your browser"
          } else {
            "Waiting for a Tailscale sign-in link"
          }
          .into(),
        );
      }
      "NeedsMachineAuth" => {
        status.message = Some("Approve this device in the Tailscale admin console".into());
      }
      _ => status.message = Some("Connecting to Tailscale".into()),
    }
    status
  }
}

fn safe_name(value: Option<&str>) -> Option<String> {
  value
    .filter(|value| !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
    .map(str::to_owned)
}

#[cfg(test)]
mod tests;
