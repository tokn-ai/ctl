//! One daemon's heartbeat interest in a shared `OpenConnect` container.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ctl_ipc::{VpnState, VpnStatus};
use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

use crate::vpn_container::{self, ContainerDescriptor, Interest, RuntimeMetadata};

mod config;
mod diagnostics;

pub(super) use config::{Config, Metadata, read_file};

use diagnostics::Diagnostics;

const IMAGE: &str = "localhost/ctl-openconnect:local";
const START_TIMEOUT: Duration = Duration::from_secs(75);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const CREATOR_ADOPTION_TIMEOUT: Duration = Duration::from_secs(3);
const IMAGE_MISSING: &str =
  "The OpenConnect container image is missing. Build it with docker/openconnect/run.sh build.";
const IMAGE_OUTDATED: &str = "The OpenConnect container image does not support shared VPN heartbeats. Rebuild it with docker/openconnect/run.sh build.";
const IMAGE_INSPECTION_FAILED: &str = "Could not verify the OpenConnect container image. Start Docker or the Podman machine and try again.";

/// Each daemon owns only its renewal task. Container lifetime is decided by
/// the in-container watchdog after every interested daemon stops renewing.
pub struct ManagedVpn {
  child: Option<Child>,
  input: Option<JoinHandle<()>>,
  interest: Option<Interest>,
  monitor: Option<JoinHandle<io::Result<ExitStatus>>>,
  container: Option<ContainerDescriptor>,
  container_name: String,
  endpoint: Option<String>,
  engine: PathBuf,
  diagnostics: Option<Diagnostics>,
  runtime: RuntimeMetadata,
  creation: Option<vpn_container::CreationGuard>,
}

impl ManagedVpn {
  #[must_use]
  pub fn status(&self) -> VpnStatus {
    let mut status = self
      .container
      .as_ref()
      .map_or_else(VpnStatus::default, ContainerDescriptor::basic_status);
    status.vpn_id = Some(self.runtime.connection_id.clone());
    status.connection_id = self
      .runtime
      .saved_profile
      .then(|| self.runtime.connection_id.clone());
    status.endpoint.clone_from(&self.endpoint);
    status.vpn_url.clone_from(&self.runtime.vpn_url);
    status.username.clone_from(&self.runtime.username);
    status.container_name = Some(self.container_name.clone());
    status.running = self.endpoint.is_some();
    status.state = if status.running {
      VpnState::Connected
    } else {
      VpnState::Starting
    };
    status.locally_connected = Some(true);
    status
  }

  /// Observes the immutable container, independently of its creator's CLI.
  ///
  /// # Errors
  /// Returns engine observation failures or an unavailable monitor.
  pub async fn exited(&mut self) -> io::Result<ExitStatus> {
    match self.monitor.as_mut() {
      Some(monitor) => monitor.await.map_err(io::Error::other)?,
      None => Err(io::Error::other(
        "OpenConnect container monitor is unavailable",
      )),
    }
  }

  pub async fn shutdown(&mut self) {
    self.interest.take();
    if let Some(monitor) = self.monitor.take() {
      monitor.abort();
    }
    if let Some(input) = self.input.take() {
      input.abort();
    }
    if let Some(child) = &mut self.child {
      let _ = child.start_kill();
      let _ = timeout(COMMAND_TIMEOUT, child.wait()).await;
    }
    self.child = None;
    if let Some(mut creation) = self.creation.take() {
      creation.cleanup().await;
    }
  }

  async fn diagnostic(&mut self) -> Option<&'static str> {
    match self.diagnostics.as_mut() {
      Some(diagnostics) => diagnostics.finish().await,
      None => None,
    }
  }

  async fn wait_ready(&mut self) -> io::Result<()> {
    match timeout(START_TIMEOUT, self.poll_ready()).await {
      Ok(result) => result,
      Err(_) => Err(io::Error::new(
        io::ErrorKind::TimedOut,
        self
          .diagnostic()
          .await
          .unwrap_or("OpenConnect VPN and SOCKS5 listener did not become ready within 75 seconds"),
      )),
    }
  }

  async fn poll_ready(&mut self) -> io::Result<()> {
    let mut adoption_deadline = None;
    loop {
      let found = vpn_container::inspect_named(&self.engine, &self.container_name).await?;
      if let Some(container) = found {
        container.compatible(&self.runtime)?;
        if container.running {
          if let Some(mut creation) = self.creation.take() {
            creation.disarm();
          }
          if self.interest.is_none()
            && let Ok(interest) = container.interest().await
          {
            self.interest = Some(interest);
          }
          if self.interest.is_some()
            && let Some(port) = container.port
          {
            let ready = timeout(
              COMMAND_TIMEOUT,
              engine_command(&self.engine)
                .args(["exec", &container.id, "/usr/local/bin/vpn-healthcheck"])
                .stdout(Stdio::null())
                .status(),
            )
            .await;
            if matches!(ready, Ok(Ok(status)) if status.success()) {
              self.endpoint = Some(format!("socks5h://127.0.0.1:{port}"));
              let monitored = container.clone();
              self.monitor = Some(tokio::spawn(async move { monitored.exited().await }));
              self.container = Some(container);
              return Ok(());
            }
          }
        } else if container.state != "created"
          && self
            .child
            .as_mut()
            .is_some_and(|child| child.try_wait().ok().flatten().is_some())
        {
          return Err(io::Error::other(self.diagnostic().await.unwrap_or(
            "OpenConnect container exited before its VPN became ready",
          )));
        }
        self.container = Some(container);
      } else if let Some(child) = &mut self.child
        && let Some(status) = child.try_wait()?
      {
        // A conflicting run can exit before the engine publishes the winner's
        // descriptor. Re-observe that reservation within a bounded window;
        // attached CLI exit alone says nothing about shared container ownership.
        let deadline =
          adoption_deadline.get_or_insert_with(|| Instant::now() + CREATOR_ADOPTION_TIMEOUT);
        if Instant::now() >= *deadline {
          let reason = self.diagnostic().await.unwrap_or(
            "Build the image with docker/openconnect/run.sh build and check the VPN settings",
          );
          return Err(io::Error::other(format!(
            "OpenConnect container exited ({status}): {reason}"
          )));
        }
      }
      sleep(Duration::from_millis(250)).await;
    }
  }
}

/// Probes readiness without acquiring or renewing interest in the container.
pub(super) async fn discovered_status(container: &ContainerDescriptor) -> VpnStatus {
  let mut status = container.basic_status();
  if container.running
    && let Some(port) = container.port
  {
    let ready = timeout(
      COMMAND_TIMEOUT,
      engine_command(&container.engine)
        .args(["exec", &container.id, "/usr/local/bin/vpn-healthcheck"])
        .stdout(Stdio::null())
        .status(),
    )
    .await;
    status.running = matches!(ready, Ok(Ok(result)) if result.success());
    status.state = if status.running {
      VpnState::Connected
    } else {
      VpnState::Starting
    };
    status.endpoint = status
      .running
      .then(|| format!("socks5h://127.0.0.1:{port}"));
  }
  status
}

impl Drop for ManagedVpn {
  fn drop(&mut self) {
    self.interest.take();
    if let Some(monitor) = &self.monitor {
      monitor.abort();
    }
    if let Some(input) = &self.input {
      input.abort();
    }
    if let Some(child) = &mut self.child {
      let _ = child.start_kill();
    }
  }
}

pub(super) fn engine_command(engine: &Path) -> Command {
  let mut command = Command::new(engine);
  command
    .stdin(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true);
  command
}

/// Starts an opt-in Array VPN from an immutable configuration snapshot.
///
/// # Errors
/// Returns engine failures or VPN readiness failures.
pub(super) async fn start(config: Config) -> io::Result<ManagedVpn> {
  start_config(config, &find_engine()?).await
}

async fn start_config(mut config: Config, engine: &Path) -> io::Result<ManagedVpn> {
  let cancellation = config.cancellation.take();
  let container_name = vpn_container::container_name(&config.runtime)?;
  let mut child = None;
  let mut diagnostics = None;
  let mut input = None;
  let mut creation = None;
  if vpn_container::inspect_named(engine, &container_name)
    .await?
    .is_none()
  {
    let image_id = verify_shared_image(engine).await?;
    let mut command = engine_command(engine);
    command.args([
      "run",
      "--rm",
      "--init",
      "--interactive",
      "--sig-proxy=false",
      "--pull=never",
      "--restart=no",
      "--name",
      &container_name,
      "--security-opt",
      "label=disable",
      "--cap-add",
      "NET_ADMIN",
      "--device",
      "/dev/net/tun",
      "--publish",
      "127.0.0.1::1080/tcp",
    ]);
    command.args(vpn_container::labels_arguments(&config.runtime)?);
    let creator = uuid::Uuid::new_v4().to_string();
    command.args(["--label", &format!("io.ctl.vpn.creator={creator}")]);
    creation = Some(vpn_container::CreationGuard::new(
      engine.to_path_buf(),
      container_name.clone(),
      creator,
    ));
    command.args([
      "--env",
      "CTLD_CONFIG_STDIN=1",
      "--tmpfs",
      "/run/secrets:rw,noexec,nosuid,nodev,mode=0700,size=65536",
    ]);
    let mut payload = zeroize::Zeroizing::new(BASE64.encode(config.content.as_bytes()));
    payload.push('\n');
    let mut created = command
      .arg(image_id)
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()?;
    diagnostics = Some(Diagnostics::new(
      created.stdout.take().expect("container stdout was piped"),
      created.stderr.take().expect("container stderr was piped"),
    ));
    let mut stdin = created.stdin.take().expect("container stdin was piped");
    input = Some(tokio::spawn(async move {
      let _ = timeout(COMMAND_TIMEOUT, stdin.write_all(payload.as_bytes())).await;
      // Config travels only once. EOF has no authority over shared lifetime.
    }));
    child = Some(created);
  }
  let mut vpn = ManagedVpn {
    child,
    input,
    interest: None,
    monitor: None,
    container: None,
    container_name,
    endpoint: None,
    engine: engine.to_path_buf(),
    diagnostics,
    runtime: config.runtime,
    creation,
  };
  let cancelled = async move {
    if let Some(cancellation) = cancellation {
      let _ = cancellation.await;
    } else {
      std::future::pending::<()>().await;
    }
  };
  let result = tokio::select! {
    result = vpn.wait_ready() => result,
    () = cancelled => Err(io::Error::new(io::ErrorKind::Interrupted, "OpenConnect startup was cancelled")),
  };
  if let Err(error) = result {
    vpn.shutdown().await;
    return Err(error);
  }
  Ok(vpn)
}

#[derive(Deserialize)]
struct ImageCapability {
  #[serde(rename = "Id", alias = "ID")]
  id: String,
  #[serde(rename = "Config")]
  config: ImageConfiguration,
}

#[derive(Deserialize)]
struct ImageConfiguration {
  #[serde(rename = "Labels")]
  labels: Option<HashMap<String, String>>,
}

async fn verify_shared_image(engine: &Path) -> io::Result<String> {
  // Runtime labels are supplied by ctld, so they cannot prove that an old local
  // image contains the shared heartbeat entrypoint. Check the image itself
  // before creating a container or handing its process any credentials.
  let output = timeout(
    COMMAND_TIMEOUT,
    engine_command(engine)
      .args(["image", "inspect", IMAGE])
      .stderr(Stdio::piped())
      .output(),
  )
  .await
  .map_err(|_| io::Error::other(IMAGE_INSPECTION_FAILED))?
  .map_err(|_| io::Error::other(IMAGE_INSPECTION_FAILED))?;
  if !output.status.success() {
    // Inspect errors can include engine addresses or supplied text. Match only
    // fixed missing-image signatures and never return raw output to the client.
    let message = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    return Err(
      if message.contains("no such image")
        || message.contains("image not known")
        || message.contains("no such object")
      {
        io::Error::new(io::ErrorKind::NotFound, IMAGE_MISSING)
      } else {
        io::Error::other(IMAGE_INSPECTION_FAILED)
      },
    );
  }
  let images: Vec<ImageCapability> = serde_json::from_slice(&output.stdout)
    .map_err(|_| io::Error::new(io::ErrorKind::Unsupported, IMAGE_OUTDATED))?;
  if images.len() != 1 {
    return Err(io::Error::other(IMAGE_INSPECTION_FAILED));
  }
  let image = images.into_iter().next().expect("one inspected image");
  if image
    .config
    .labels
    .as_ref()
    .and_then(|labels| labels.get(vpn_container::LABEL_PROTOCOL))
    .map(String::as_str)
    != Some(vpn_container::PROTOCOL)
  {
    return Err(io::Error::new(io::ErrorKind::Unsupported, IMAGE_OUTDATED));
  }
  // Freeze the verified image contents across any concurrent tag replacement.
  let id = image.id.strip_prefix("sha256:").unwrap_or(&image.id);
  if id.len() != 64
    || !id
      .bytes()
      .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
  {
    return Err(io::Error::other(IMAGE_INSPECTION_FAILED));
  }
  Ok(id.into())
}

pub(super) fn find_engine() -> io::Result<PathBuf> {
  engine_candidates(
    std::env::var_os("PATH").as_deref(),
    dirs::home_dir().as_deref(),
  )
  .into_iter()
  .find(|path| {
    let Ok(metadata) = path.metadata() else {
      return false;
    };
    if !metadata.is_file() {
      return false;
    }
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
      true
    }
  })
  .ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::NotFound,
      "Docker or Podman was not found; install and start a container engine",
    )
  })
}

fn engine_candidates(search_path: Option<&OsStr>, home: Option<&Path>) -> Vec<PathBuf> {
  let mut directories: Vec<PathBuf> = search_path
    .into_iter()
    .flat_map(std::env::split_paths)
    .filter(|path| path.is_absolute())
    .collect();
  if let Some(home) = home.filter(|path| path.is_absolute()) {
    directories.push(home.join(".local/bin"));
  }
  directories
    .extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
  let mut candidates: Vec<_> = ["docker", "podman"]
    .into_iter()
    .flat_map(|name| {
      directories
        .iter()
        .map(move |directory| directory.join(name))
    })
    .collect();
  #[cfg(target_os = "macos")]
  candidates.extend([
    PathBuf::from("/Applications/Docker.app/Contents/Resources/bin/docker"),
    PathBuf::from("/opt/podman/bin/podman"),
  ]);
  let mut seen = HashSet::new();
  candidates.retain(|path| seen.insert(path.clone()));
  candidates
}

#[derive(Deserialize)]
struct PortBinding {
  #[serde(rename = "HostIp")]
  host_ip: String,
  #[serde(rename = "HostPort")]
  host_port: String,
}

pub(super) fn parse_published_port(bytes: &[u8]) -> io::Result<u16> {
  let bindings: HashMap<String, Option<Vec<PortBinding>>> = serde_json::from_slice(bytes)?;
  let bindings = bindings
    .get("1080/tcp")
    .and_then(Option::as_deref)
    .filter(|bindings| bindings.len() == 1)
    .ok_or_else(|| io::Error::other("expected one published SOCKS5 port"))?;
  let binding = &bindings[0];
  if binding.host_ip != "127.0.0.1" {
    return Err(io::Error::other("SOCKS5 port was not bound to localhost"));
  }
  binding
    .host_port
    .parse::<u16>()
    .ok()
    .filter(|port| *port != 0)
    .ok_or_else(|| io::Error::other("invalid published SOCKS5 port"))
}

#[cfg(test)]
mod tests;
