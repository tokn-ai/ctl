//! Shared container discovery and independent, expiring daemon interests.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use ctld_ipc::{VpnConnection, VpnProvider, VpnSettings, VpnState, VpnStatus};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{interval, sleep, timeout};

use crate::openconnect::{engine_command, parse_published_port};

pub(super) const HEARTBEAT_SCRIPT: &str = include_str!("../../../docker/vpn/heartbeat.sh");
pub(super) const WATCHDOG_SCRIPT: &str = include_str!("../../../docker/vpn/watchdog.sh");
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const INITIAL_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(3);
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_CONTAINERS: usize = 128;
const INSPECT_CONCURRENCY: usize = 16;
pub(super) const PROTOCOL: &str = "1";
pub(super) const LABEL_PROTOCOL: &str = "io.ctl.vpn.protocol";
const LABEL_USER: &str = "io.ctl.vpn.user";
const LABEL_ID: &str = "io.ctl.vpn.id";
const LABEL_METADATA: &str = "io.ctl.vpn.metadata";

/// Compatibility metadata exposes only the gateway origin and a routing-settings
/// fingerprint. The authentication password never participates in that key.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct RuntimeMetadata {
  pub connection_id: String,
  pub saved_profile: bool,
  pub provider: VpnProvider,
  pub settings_key: String,
  pub vpn_url: Option<String>,
  pub username: Option<String>,
}

impl RuntimeMetadata {
  pub(super) fn for_connection(connection: &VpnConnection) -> io::Result<Self> {
    connection.validate().map_err(invalid)?;
    let (public_settings, vpn_url, username) = match &connection.settings {
      VpnSettings::Openconnect {
        url,
        username,
        auth_method,
        target_ip,
        ..
      } => {
        let origin = public_origin(url)?;
        (
          serde_json::json!([
            "openconnect",
            routing_url(url)?,
            username,
            auth_method,
            target_ip
          ]),
          Some(origin),
          Some(username.clone()),
        )
      }
      VpnSettings::Tailscale {
        hostname,
        accept_routes,
      } => (
        serde_json::json!(["tailscale", hostname, accept_routes]),
        None,
        None,
      ),
    };
    Ok(Self {
      connection_id: connection.connection_id.clone(),
      saved_profile: true,
      provider: connection.provider(),
      settings_key: digest(&serde_json::to_vec(&public_settings)?),
      vpn_url,
      username,
    })
  }

  pub(super) fn for_env(
    identity: &Path,
    content: &str,
    vpn_url: Option<String>,
    username: Option<String>,
  ) -> io::Result<Self> {
    let fields: HashMap<_, _> = content
      .lines()
      .filter_map(|line| line.split_once('='))
      .filter(|(key, _)| *key != "VPN_PASSWORD")
      .collect();
    let routing = fields
      .get("VPN_URL")
      .map(|url| routing_url(url))
      .transpose()?;
    let vpn_url = vpn_url.map(|url| public_origin(&url)).transpose()?;
    let public_settings = serde_json::json!([
      "openconnect",
      routing,
      username,
      fields.get("VPN_AUTH_METHOD"),
      fields.get("TARGET_IP")
    ]);
    Ok(Self {
      connection_id: format!("env-{}", digest(identity.as_os_str().as_encoded_bytes())),
      saved_profile: false,
      provider: VpnProvider::Openconnect,
      settings_key: digest(&serde_json::to_vec(&public_settings)?),
      vpn_url,
      username,
    })
  }
}

fn public_origin(value: &str) -> io::Result<String> {
  Ok(parsed_gateway(value)?.origin().ascii_serialization())
}

// Paths and query parameters can select a different authentication gateway. They
// participate in compatibility without exposing their plaintext in labels. URL
// userinfo is authentication material and is excluded like VPN_PASSWORD.
fn routing_url(value: &str) -> io::Result<String> {
  let mut parsed = parsed_gateway(value)?;
  let _ = parsed.set_username("");
  let _ = parsed.set_password(None);
  Ok(parsed.into())
}

fn parsed_gateway(value: &str) -> io::Result<url::Url> {
  let value = if value.starts_with("https://") {
    value.to_owned()
  } else {
    format!("https://{value}")
  };
  let parsed = url::Url::parse(&value).map_err(|_| invalid("Invalid VPN gateway"))?;
  if parsed.scheme() != "https" || parsed.host_str().is_none() {
    return Err(invalid("Invalid VPN gateway"));
  }
  Ok(parsed)
}

fn digest(value: &[u8]) -> String {
  format!("{:x}", Sha256::digest(value))
}
fn invalid(message: impl Into<String>) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
fn owner() -> io::Result<PathBuf> {
  dirs::home_dir().ok_or_else(|| io::Error::other("Could not locate the VPN owner"))
}
pub(super) fn namespace() -> io::Result<String> {
  Ok(digest(owner()?.as_os_str().as_encoded_bytes()))
}

pub(super) fn container_name(metadata: &RuntimeMetadata) -> io::Result<String> {
  let mut key = owner()?.as_os_str().as_encoded_bytes().to_vec();
  key.push(0);
  key.extend(metadata.connection_id.as_bytes());
  let provider = match metadata.provider {
    VpnProvider::Openconnect => "openconnect",
    VpnProvider::Tailscale => "tailscale",
  };
  Ok(format!("ctld-{provider}-{}", digest(&key)))
}

pub(super) fn labels_arguments(metadata: &RuntimeMetadata) -> io::Result<Vec<String>> {
  Ok(
    [
      (LABEL_PROTOCOL, PROTOCOL.to_owned()),
      (LABEL_USER, namespace()?),
      (LABEL_ID, metadata.connection_id.clone()),
      (LABEL_METADATA, serde_json::to_string(metadata)?),
    ]
    .into_iter()
    .flat_map(|(key, value)| ["--label".into(), format!("{key}={value}")])
    .collect(),
  )
}

#[derive(Clone, Debug)]
pub(super) struct ContainerDescriptor {
  pub engine: PathBuf,
  pub id: String,
  pub name: String,
  pub metadata: RuntimeMetadata,
  pub port: Option<u16>,
  pub running: bool,
  pub exit_code: i32,
  pub state: String,
  pub labels: HashMap<String, String>,
  pub shared_supported: bool,
  healthy: bool,
}

impl ContainerDescriptor {
  pub(super) fn compatible(&self, metadata: &RuntimeMetadata) -> io::Result<()> {
    if self.shared_supported && self.metadata == *metadata {
      Ok(())
    } else {
      Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "This VPN is already running with different public settings; disconnect it before applying changes",
      ))
    }
  }

  pub(super) async fn interest(&self) -> io::Result<Interest> {
    heartbeat(&self.engine, &self.id).await
  }

  pub(super) fn basic_status(&self) -> VpnStatus {
    let connected = self.running && self.port.is_some() && self.healthy;
    VpnStatus {
      provider: self.metadata.provider,
      vpn_id: Some(self.metadata.connection_id.clone()),
      connection_id: self
        .metadata
        .saved_profile
        .then(|| self.metadata.connection_id.clone()),
      container_name: Some(self.name.clone()),
      container_id: Some(self.id.clone()),
      shared_container: true,
      locally_connected: Some(false),
      vpn_url: self.metadata.vpn_url.clone(),
      username: self.metadata.username.clone(),
      endpoint: if connected {
        self.port.map(|port| format!("socks5h://127.0.0.1:{port}"))
      } else {
        None
      },
      running: connected,
      state: if connected {
        VpnState::Connected
      } else if self.running || self.state == "created" {
        VpnState::Starting
      } else {
        VpnState::Stopped
      },
      ..VpnStatus::default()
    }
  }

  /// Polls immutable identity, preserving interest through temporary engine failures.
  pub(super) async fn exited(&self) -> io::Result<ExitStatus> {
    loop {
      match inspect_named(&self.engine, &self.id).await {
        Ok(Some(current)) if !current.running && current.state != "created" => {
          return Ok(exit_status(current.exit_code));
        }
        Ok(None) => return Ok(exit_status(0)),
        _ => sleep(Duration::from_millis(500)).await,
      }
    }
  }
}

#[cfg(unix)]
fn exit_status(code: i32) -> ExitStatus {
  use std::os::unix::process::ExitStatusExt as _;
  ExitStatus::from_raw(code.clamp(0, 255) << 8)
}
#[cfg(windows)]
fn exit_status(code: i32) -> ExitStatus {
  use std::os::windows::process::ExitStatusExt as _;
  ExitStatus::from_raw(code.try_into().unwrap_or(1))
}

pub(super) struct Interest {
  task: JoinHandle<()>,
}
impl Drop for Interest {
  fn drop(&mut self) {
    self.task.abort();
  }
}

pub(super) async fn heartbeat(engine: &Path, id: &str) -> io::Result<Interest> {
  if !immutable_id(id) {
    return Err(invalid("Expected an immutable VPN container ID"));
  }
  // A just-started Tailscale image first installs these embedded scripts. Engine
  // Running/port metadata can appear slightly before that bootstrap completes.
  timeout(INITIAL_HEARTBEAT_TIMEOUT, async {
    loop {
      if beat(engine, id).await.is_ok() {
        break;
      }
      sleep(Duration::from_millis(100)).await;
    }
  })
  .await
  .map_err(|_| io::Error::other("The shared VPN container did not accept its first heartbeat"))?;
  let engine = engine.to_path_buf();
  let id = id.to_owned();
  let task = tokio::spawn(async move {
    let mut ticks = interval(HEARTBEAT_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticks.tick().await;
    loop {
      ticks.tick().await;
      let _ = beat(&engine, &id).await;
    }
  });
  Ok(Interest { task })
}

async fn beat(engine: &Path, id: &str) -> io::Result<()> {
  if !immutable_id(id) {
    return Err(invalid("Expected an immutable VPN container ID"));
  }
  let status = timeout(
    COMMAND_TIMEOUT,
    engine_command(engine)
      .args(["exec", id, "/bin/sh", "/run/ctl/heartbeat.sh"])
      .stdout(Stdio::null())
      .status(),
  )
  .await??;
  if status.success() {
    Ok(())
  } else {
    Err(io::Error::other(
      "The shared VPN container no longer accepts heartbeats",
    ))
  }
}

pub(super) async fn list(engine: &Path) -> io::Result<Vec<ContainerDescriptor>> {
  timeout(INVENTORY_TIMEOUT, list_bounded(engine)).await?
}

async fn list_bounded(engine: &Path) -> io::Result<Vec<ContainerDescriptor>> {
  let ids = inventory_ids(
    engine,
    &[
      format!("label={LABEL_PROTOCOL}={PROTOCOL}"),
      format!("label={LABEL_USER}={}", namespace()?),
    ],
  )
  .await?;
  let mut containers = inspect_inventory(engine, ids).await?;
  containers.sort_by(|left, right| left.id.cmp(&right.id));
  Ok(containers)
}

async fn inventory_ids(engine: &Path, filters: &[String]) -> io::Result<Vec<String>> {
  let mut command = engine_command(engine);
  command.args(["ps", "--all", "--quiet", "--no-trunc"]);
  for filter in filters {
    command.args(["--filter", filter]);
  }
  let output = timeout(COMMAND_TIMEOUT, command.output()).await??;
  if !output.status.success() {
    return Err(io::Error::other("Could not list managed VPN containers"));
  }
  let ids =
    std::str::from_utf8(&output.stdout).map_err(|_| invalid("Invalid container inventory"))?;
  let mut unique = HashSet::new();
  let mut pending = Vec::new();
  for id in ids.lines().filter(|id| !id.is_empty()) {
    if !immutable_id(id) {
      return Err(invalid("Invalid container identity"));
    }
    if unique.insert(id) {
      pending.push(id.to_owned());
    }
    if pending.len() > MAX_CONTAINERS {
      return Err(io::Error::other(
        "Managed VPN container inventory exceeds its safety limit",
      ));
    }
  }
  Ok(pending)
}

async fn inspect_inventory(
  engine: &Path,
  ids: Vec<String>,
) -> io::Result<Vec<ContainerDescriptor>> {
  let mut active = JoinSet::new();
  let mut pending = ids.into_iter();
  let mut containers = Vec::new();
  loop {
    while active.len() < INSPECT_CONCURRENCY {
      let Some(id) = pending.next() else {
        break;
      };
      let engine = engine.to_path_buf();
      active.spawn(async move { inspect_named(&engine, &id).await });
    }
    let Some(result) = active.join_next().await else {
      break;
    };
    if let Some(container) = result.map_err(io::Error::other)?? {
      containers.push(container);
    }
  }
  Ok(containers)
}

pub(super) async fn inspect_named(
  engine: &Path,
  name: &str,
) -> io::Result<Option<ContainerDescriptor>> {
  inspect_bytes(engine, name)
    .await?
    .map(|bytes| parse_descriptor(engine, name, &bytes))
    .transpose()
}

async fn inspect_bytes(engine: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
  let output = timeout(
    COMMAND_TIMEOUT,
    engine_command(engine)
      .args(["container", "inspect", name])
      .stderr(Stdio::piped())
      .output(),
  )
  .await??;
  if !output.status.success() {
    // Never mistake a stopped engine for an absent container. Docker/Podman
    // use these exact English markers for a confirmed missing object.
    let message = String::from_utf8_lossy(&output.stderr);
    if message.contains("No such container")
      || message.contains("no such container")
      || message.contains("No such object")
      || message.contains("no such object")
    {
      return Ok(None);
    }
    return Err(io::Error::other(
      "Could not inspect the VPN container engine",
    ));
  }
  Ok(Some(output.stdout))
}

#[derive(Deserialize)]
struct Inspection {
  #[serde(rename = "Id", alias = "ID")]
  id: String,
  #[serde(rename = "Name")]
  name: String,
  #[serde(rename = "Config")]
  config: InspectionConfig,
  #[serde(rename = "State")]
  state: InspectionState,
  #[serde(rename = "NetworkSettings")]
  network: InspectionNetwork,
}
#[derive(Deserialize)]
struct InspectionConfig {
  #[serde(rename = "Labels", default)]
  labels: HashMap<String, String>,
}
#[derive(Deserialize)]
struct InspectionState {
  #[serde(rename = "Running")]
  running: bool,
  #[serde(rename = "ExitCode", default)]
  exit_code: i32,
  #[serde(rename = "Status", default)]
  status: String,
  #[serde(rename = "Health", default)]
  health: Option<InspectionHealth>,
}
#[derive(Deserialize)]
struct InspectionHealth {
  #[serde(rename = "Status")]
  status: String,
}
#[derive(Deserialize)]
struct InspectionNetwork {
  #[serde(rename = "Ports", default)]
  ports: serde_json::Value,
}

fn parse_descriptor(
  engine: &Path,
  requested: &str,
  bytes: &[u8],
) -> io::Result<ContainerDescriptor> {
  let mut values: Vec<Inspection> = serde_json::from_slice(bytes)?;
  if values.len() != 1 {
    return Err(invalid("Expected one managed VPN container"));
  }
  let raw = values.pop().expect("one inspected container");
  let labels = &raw.config.labels;
  if labels.get(LABEL_PROTOCOL).map(String::as_str) != Some(PROTOCOL) {
    return Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "This VPN container cannot share heartbeats; disconnect and recreate legacy VPN containers before sharing",
    ));
  }
  if !immutable_id(&raw.id)
    || (immutable_id(requested) && raw.id != requested)
    || labels.get(LABEL_USER).map(String::as_str) != Some(namespace()?.as_str())
  {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "The container is not a shared VPN owned by this user",
    ));
  }
  let metadata: RuntimeMetadata = serde_json::from_str(
    labels
      .get(LABEL_METADATA)
      .ok_or_else(|| invalid("VPN runtime metadata is missing"))?,
  )?;
  let name = raw.name.trim_start_matches('/').to_owned();
  if labels.get(LABEL_ID) != Some(&metadata.connection_id) || name != container_name(&metadata)? {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "The VPN container profile does not match its reservation",
    ));
  }
  let port = parse_published_port(&serde_json::to_vec(&raw.network.ports)?).ok();
  Ok(ContainerDescriptor {
    engine: engine.to_path_buf(),
    id: raw.id,
    name,
    metadata,
    port,
    running: raw.state.running,
    exit_code: raw.state.exit_code,
    state: raw.state.status,
    labels: raw.config.labels,
    shared_supported: true,
    healthy: raw
      .state
      .health
      .is_some_and(|health| health.status == "healthy"),
  })
}

fn immutable_id(id: &str) -> bool {
  id.len() == 64
    && id
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Cancellation cleanup is limited to our still-unstarted create reservation.
pub(super) struct CreationGuard {
  engine: PathBuf,
  name: String,
  token: String,
  armed: bool,
}
impl CreationGuard {
  pub(super) fn new(engine: PathBuf, name: String, token: String) -> Self {
    Self {
      engine,
      name,
      token,
      armed: true,
    }
  }
  pub(super) fn disarm(&mut self) {
    self.armed = false;
  }

  /// Await reservation cleanup before a normal cancellation shuts down Tokio.
  pub(super) async fn cleanup(&mut self) {
    if self.armed {
      cleanup_reservation(&self.engine, &self.name, &self.token).await;
      self.armed = false;
    }
  }
}
impl Drop for CreationGuard {
  fn drop(&mut self) {
    if !self.armed {
      return;
    }
    let engine = self.engine.clone();
    let name = self.name.clone();
    let token = self.token.clone();
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
      return;
    };
    runtime.spawn(async move {
      cleanup_reservation(&engine, &name, &token).await;
    });
  }
}

async fn cleanup_reservation(engine: &Path, name: &str, token: &str) {
  let _ = timeout(Duration::from_secs(3), async {
    loop {
      match inspect_named(engine, name).await {
        Ok(Some(container))
          if !container.running
            && container.state == "created"
            && container
              .labels
              .get("io.ctl.vpn.creator")
              .map(String::as_str)
              == Some(token) =>
        {
          // An adopter may start the reservation between inspection and removal.
          // Non-force removal then refuses to remove that running container.
          let _ = engine_command(engine)
            .args(["rm", &container.id])
            .stdout(Stdio::null())
            .status()
            .await;
          return;
        }
        Ok(Some(_)) => return,
        _ => sleep(Duration::from_millis(100)).await,
      }
    }
  })
  .await;
}

#[cfg(all(test, unix))]
mod tests;
