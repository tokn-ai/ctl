//! Persistent Tailscale identities in independently heartbeated shared containers.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use ctl_ipc::{VpnConnection, VpnProvider, VpnSettings, VpnState, VpnStatus};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep, timeout};

use crate::openconnect::{engine_command, find_engine};
use crate::vpn_container::{self, ContainerDescriptor, CreationGuard, Interest, RuntimeMetadata};

mod socks;
use socks::ready as socks_ready;

const IMAGE: &str = "docker.io/tailscale/tailscale:v1.94.2";
const ENTRYPOINT: &str = include_str!("../../../docker/tailscale/entrypoint.sh");
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const START_TIMEOUT: Duration = Duration::from_mins(2);
const ADOPTION_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) struct Config {
  container_name: String,
  hostname: String,
  accept_routes: bool,
  runtime: RuntimeMetadata,
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
        .unwrap_or_else(|| format!("ctmux-{}", &container_name[15..27])),
      container_name,
      accept_routes: *accept_routes,
      runtime: RuntimeMetadata::for_connection(connection)?,
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
  interest: Option<Interest>,
  monitor: Option<JoinHandle<io::Result<ExitStatus>>>,
  status: watch::Receiver<VpnStatus>,
  container: ContainerDescriptor,
}

impl ManagedVpn {
  pub(super) fn status(&self) -> VpnStatus {
    self.status.borrow().clone()
  }

  pub(super) async fn exited(&mut self) -> io::Result<ExitStatus> {
    // The actor polls and drops this future repeatedly. Retaining the monitor
    // task preserves the immutable-ID inspection and its timers between polls.
    let result = self
      .monitor
      .as_mut()
      .expect("ready VPN has a monitor")
      .await
      .map_err(io::Error::other);
    // A completed JoinHandle cannot be polled again during later shutdown.
    self.monitor.take();
    result?
  }

  pub(super) async fn shutdown(&mut self) {
    self.interest.take();
    if let Some(monitor) = self.monitor.take() {
      monitor.abort();
      let _ = monitor.await;
    }
  }

  fn start_monitor(&mut self, updates: watch::Sender<VpnStatus>, port: u16) {
    let container = self.container.clone();
    self.monitor = Some(tokio::spawn(async move {
      let mut exited = Box::pin(container.exited());
      let mut ticks = interval(Duration::from_secs(2));
      ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
      loop {
        tokio::select! {
          result = &mut exited => return result,
          _ = ticks.tick() => {
            let status = discovered_status(&container.engine, &container.id, &container.name, port).await;
            if updates.send(container_status(&container, status)).is_err() {
              return Err(io::Error::new(io::ErrorKind::Interrupted, "Tailscale status monitor was released"));
            }
          }
        }
      }
    }));
  }

  async fn wait_ready(&self) -> io::Result<(u16, BackendStatus)> {
    let mut exited = Box::pin(self.container.exited());
    let ready = async {
      loop {
        if let Some(port) = self.container.port
          && let Ok(status) = backend_status(&self.container.engine, &self.container.id).await
          && status.is_actionable()
          && socks_ready(port).await.is_ok()
        {
          return Ok((port, status));
        }
        sleep(Duration::from_millis(250)).await;
      }
    };
    tokio::select! {
      result = ready => result,
      result = &mut exited => {
        result?;
        Err(io::Error::other("Tailscale container exited before becoming ready"))
      }
    }
  }
}

impl Drop for ManagedVpn {
  fn drop(&mut self) {
    self.interest.take();
    if let Some(monitor) = &self.monitor {
      monitor.abort();
    }
  }
}

fn container_status(container: &ContainerDescriptor, mut status: VpnStatus) -> VpnStatus {
  let metadata = container.basic_status();
  status.vpn_id = metadata.vpn_id;
  status.connection_id = metadata.connection_id;
  status.container_id = Some(container.id.clone());
  status.shared_container = container.shared_supported;
  status.locally_connected = Some(true);
  status
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

async fn start_config(mut config: Config, engine: &Path) -> io::Result<ManagedVpn> {
  let cancellation = config.cancellation.take();
  let mut reservation = None;
  let result = {
    let startup = async {
      let container = prepare_container(&config, engine, &mut reservation).await?;
      let interest = container.interest().await?;
      let (updates, status) = watch::channel(container_status(
        &container,
        pending_status(&container.name),
      ));
      let mut vpn = ManagedVpn {
        interest: Some(interest),
        monitor: None,
        status,
        container,
      };
      let (port, backend) = vpn.wait_ready().await?;
      updates.send_replace(container_status(
        &vpn.container,
        backend.status(&vpn.container.name, port),
      ));
      vpn.start_monitor(updates, port);
      Ok(vpn)
    };
    tokio::select! {
      result = timeout(START_TIMEOUT, startup) => result.unwrap_or_else(|_| Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Tailscale container did not start within 120 seconds; check the container engine and image download",
      ))),
      () = cancelled(cancellation) => Err(io::Error::new(io::ErrorKind::Interrupted, "Tailscale startup was cancelled")),
    }
  };
  if result.is_err()
    && let Some(reservation) = &mut reservation
  {
    reservation.cleanup().await;
  }
  result
}

async fn prepare_container(
  config: &Config,
  engine: &Path,
  reservation: &mut Option<CreationGuard>,
) -> io::Result<ContainerDescriptor> {
  if let Some(container) = vpn_container::inspect_named(engine, &config.container_name).await? {
    container.compatible(&config.runtime)?;
    return await_running(config, engine).await;
  }
  // The engine atomically reserves the stable profile name. A losing creator
  // adopts the compatible winner rather than opening a second identity volume.
  let token = uuid::Uuid::new_v4().to_string();
  *reservation = Some(CreationGuard::new(
    engine.to_path_buf(),
    config.container_name.clone(),
    token.clone(),
  ));
  let result = run_container(config, engine, &token).await;
  match await_running(config, engine).await {
    Ok(container) => {
      reservation
        .as_mut()
        .expect("create reservation exists")
        .disarm();
      Ok(container)
    }
    Err(error) => match result {
      Err(create_error) if error.kind() == io::ErrorKind::TimedOut => Err(create_error),
      _ => Err(error),
    },
  }
}

async fn await_running(config: &Config, engine: &Path) -> io::Result<ContainerDescriptor> {
  timeout(ADOPTION_TIMEOUT, async {
    loop {
      if let Some(container) = vpn_container::inspect_named(engine, &config.container_name).await? {
        container.compatible(&config.runtime)?;
        if container.running && container.port.is_some() {
          return Ok(container);
        }
        if container.state != "created" && !container.running {
          return Err(io::Error::other(
            "Tailscale container is stopped; recreate it before connecting",
          ));
        }
      }
      sleep(Duration::from_millis(50)).await;
    }
  })
  .await
  .map_err(|_| {
    io::Error::new(
      io::ErrorKind::TimedOut,
      "Tailscale shared container did not become available",
    )
  })?
}

async fn run_container(config: &Config, engine: &Path, token: &str) -> io::Result<()> {
  let volume = format!("{}-state:/state", config.container_name);
  let mut command = engine_command(engine);
  command.args([
    "run",
    "--detach",
    "--rm",
    "--init",
    "--pull=missing",
    "--restart=no",
    "--name",
    &config.container_name,
    "--label",
    &format!("io.ctl.vpn.creator={token}"),
    "--publish",
    "127.0.0.1::1080/tcp",
    "--volume",
    &volume,
    "--env",
    &format!("CTLD_HOSTNAME={}", config.hostname),
    "--env",
    &format!("CTLD_ACCEPT_ROUTES={}", config.accept_routes),
  ]);
  command.args(vpn_container::labels_arguments(&config.runtime)?);
  let output = command
    .args(["--entrypoint", "/bin/sh", IMAGE, "-c", &entrypoint()])
    .stdout(Stdio::piped())
    .output()
    .await?;
  if !output.status.success() {
    return Err(io::Error::other(
      "Could not create the Tailscale container; check the engine and image availability",
    ));
  }
  Ok(())
}

fn entrypoint() -> String {
  format!(
    "mkdir -p /run/ctl; chmod 700 /run/ctl\ncat > /run/ctl/heartbeat.sh <<'CTLD_HEARTBEAT'\n{}\nCTLD_HEARTBEAT\ncat > /run/ctl/watchdog.sh <<'CTLD_WATCHDOG'\n{}\nCTLD_WATCHDOG\n{}",
    vpn_container::HEARTBEAT_SCRIPT,
    vpn_container::WATCHDOG_SCRIPT,
    ENTRYPOINT,
  )
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

/// Observes a discovered container without acquiring a heartbeat interest.
pub(super) async fn discovered_status(
  engine: &Path,
  container_id: &str,
  container_name: &str,
  port: u16,
) -> VpnStatus {
  if let Ok(backend) = backend_status(engine, container_id).await {
    let status = backend.status(container_name, port);
    if status.running && socks_ready(port).await.is_err() {
      let mut pending = pending_status(container_name);
      pending.message = Some("Waiting for the Tailscale SOCKS5 proxy".into());
      pending
    } else {
      status
    }
  } else {
    let mut pending = pending_status(container_name);
    pending.message = Some("Waiting for the Tailscale service".into());
    pending
  }
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
      || (self.backend_state == "NeedsLogin" && ctl_ipc::vpn::is_tailscale_auth_url(&self.auth_url))
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
          ctl_ipc::vpn::is_tailscale_auth_url(&self.auth_url).then(|| self.auth_url.clone());
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
