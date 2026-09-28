//! A container lease owned by ctld, including when the host process crashes.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ctld_ipc::{VpnConnection, VpnState, VpnStatus};
use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep, timeout};
use zeroize::Zeroizing;

mod diagnostics;

use diagnostics::Diagnostics;

const IMAGE: &str = "localhost/ctl-openconnect:local";
const START_TIMEOUT: Duration = Duration::from_secs(75);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

pub struct Options {
  pub env_file: PathBuf,
}

/// Dropping the lease stops heartbeats even when startup is cancelled. The
/// container's own watchdog also works if ctld or the attached CLI receives SIGKILL.
pub struct ManagedVpn {
  child: Child,
  heartbeat: JoinHandle<()>,
  container_name: String,
  endpoint: Option<String>,
  engine: PathBuf,
  diagnostics: Diagnostics,
}

impl ManagedVpn {
  #[must_use]
  pub fn status(&self) -> VpnStatus {
    VpnStatus {
      endpoint: self.endpoint.clone(),
      container_name: Some(self.container_name.clone()),
      running: self.endpoint.is_some(),
      connection_id: None,
      state: if self.endpoint.is_some() {
        VpnState::Connected
      } else {
        VpnState::Stopped
      },
    }
  }

  /// Waits for the attached container process to exit.
  ///
  /// # Errors
  /// Returns an error if the container CLI cannot be reaped.
  pub async fn exited(&mut self) -> io::Result<ExitStatus> {
    self.child.wait().await
  }

  pub async fn shutdown(&mut self) {
    self.endpoint = None;
    self.heartbeat.abort();
    // Explicit removal makes normal shutdown prompt; the in-container lease
    // timeout remains the fallback if the engine is unreachable.
    let _ = timeout(
      COMMAND_TIMEOUT,
      engine_command(&self.engine)
        .args(["rm", "--force", &self.container_name])
        .stdout(Stdio::null())
        .status(),
    )
    .await;
    let _ = self.child.start_kill();
    let _ = timeout(COMMAND_TIMEOUT, self.child.wait()).await;
  }

  async fn wait_ready(&mut self) -> io::Result<()> {
    if let Ok(result) = timeout(START_TIMEOUT, self.poll_ready()).await {
      result
    } else {
      let reason = self
        .diagnostics
        .finish()
        .await
        .unwrap_or("OpenConnect VPN and SOCKS5 listener did not become ready within 75 seconds");
      Err(io::Error::new(io::ErrorKind::TimedOut, reason))
    }
  }

  async fn poll_ready(&mut self) -> io::Result<()> {
    loop {
      if let Some(status) = self.child.try_wait()? {
        if let Some(reason) = self.diagnostics.finish().await {
          return Err(io::Error::other(format!(
            "OpenConnect container exited ({status}): {reason}"
          )));
        }
        return Err(io::Error::other(format!(
          "OpenConnect container exited ({status}); build the image with docker/openconnect/run.sh build and check the VPN settings"
        )));
      }
      if let Ok(port) = self.published_port().await {
        let ready = timeout(
          COMMAND_TIMEOUT,
          engine_command(&self.engine)
            .args([
              "exec",
              &self.container_name,
              "/usr/local/bin/vpn-healthcheck",
            ])
            .stdout(Stdio::null())
            .status(),
        )
        .await;
        if matches!(ready, Ok(Ok(status)) if status.success()) {
          self.endpoint = Some(format!("socks5h://127.0.0.1:{port}"));
          return Ok(());
        }
      }
      sleep(Duration::from_millis(500)).await;
    }
  }

  async fn published_port(&self) -> io::Result<u16> {
    let output = timeout(
      COMMAND_TIMEOUT,
      engine_command(&self.engine)
        .args([
          "inspect",
          "--format",
          "{{json .NetworkSettings.Ports}}",
          &self.container_name,
        ])
        .output(),
    )
    .await??;
    if !output.status.success() {
      return Err(io::Error::other("container port is not available yet"));
    }
    parse_published_port(&output.stdout)
  }
}

impl Drop for ManagedVpn {
  fn drop(&mut self) {
    self.heartbeat.abort();
    let _ = self.child.start_kill();
  }
}

fn engine_command(engine: &Path) -> Command {
  let mut command = Command::new(engine);
  command
    .stdin(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true);
  command
}

/// Starts an opt-in Array VPN using the locally built image.
///
/// # Errors
/// Returns an error for unsafe/missing configuration, engine failure, or a VPN
/// that does not finish configuring its routes and SOCKS5 listener in time.
pub async fn start(options: Options) -> io::Result<ManagedVpn> {
  start_with_engine(options, &find_engine()?).await
}

/// Starts saved connection settings without creating a host-side secret file.
///
/// # Errors
/// Returns sanitized validation errors, engine failures, or readiness failures.
pub async fn start_connection(connection: VpnConnection) -> io::Result<ManagedVpn> {
  let config = connection_env(&connection)?;
  drop(connection);
  start_config(Config::Inline(config), &find_engine()?).await
}

enum Config {
  File(PathBuf),
  Inline(Zeroizing<String>),
}

async fn start_with_engine(options: Options, engine: &Path) -> io::Result<ManagedVpn> {
  start_config(Config::File(options.env_file), engine).await
}

async fn start_config(config: Config, engine: &Path) -> io::Result<ManagedVpn> {
  let container_name = format!("ctld-openconnect-{}", uuid::Uuid::new_v4().simple());
  let mut command = engine_command(engine);
  command.args([
    "run",
    "--rm",
    "--init",
    "--interactive",
    "--pull=never",
    "--restart=no",
    "--name",
    &container_name,
    "--label",
    "io.ctl.service=openconnect",
    "--security-opt",
    "label=disable",
    "--cap-add",
    "NET_ADMIN",
    "--device",
    "/dev/net/tun",
    "--publish",
    "127.0.0.1::1080/tcp",
  ]);
  let initial_payload = match config {
    Config::File(path) => {
      let path = prepare_config(&path)?;
      command.args([
        "--mount",
        &format!("type=bind,source={path},target=/run/secrets/openconnect.env,readonly"),
      ]);
      None
    }
    Config::Inline(config) => {
      command.args([
        "--env",
        "CTLD_CONFIG_STDIN=1",
        "--tmpfs",
        "/run/secrets:rw,noexec,nosuid,nodev,mode=0700,size=65536",
      ]);
      let mut payload = Zeroizing::new(BASE64.encode(config.as_bytes()));
      payload.push('\n');
      Some(payload)
    }
  };
  let mut child = command
    .arg(IMAGE)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;
  let diagnostics = Diagnostics::new(
    child.stdout.take().expect("container stdout was piped"),
    child.stderr.take().expect("container stderr was piped"),
  );
  let mut stdin = child.stdin.take().expect("container stdin was piped");
  let heartbeat = tokio::spawn(async move {
    if let Some(payload) = initial_payload {
      if !matches!(
        timeout(COMMAND_TIMEOUT, stdin.write_all(payload.as_bytes())).await,
        Ok(Ok(()))
      ) {
        return;
      }
      // Drop and zeroize the credentials before entering the heartbeat loop.
      drop(payload);
    }
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
  let mut vpn = ManagedVpn {
    child,
    heartbeat,
    container_name,
    endpoint: None,
    engine: engine.to_path_buf(),
    diagnostics,
  };
  if let Err(error) = vpn.wait_ready().await {
    vpn.shutdown().await;
    return Err(error);
  }
  Ok(vpn)
}

fn connection_env(connection: &VpnConnection) -> io::Result<Zeroizing<String>> {
  connection
    .validate()
    .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
  let mut config = Zeroizing::new(String::new());
  for (key, value) in [
    ("VPN_URL", connection.url.as_str()),
    ("VPN_USERNAME", connection.username.as_str()),
    ("VPN_PASSWORD", connection.password.as_str()),
    (
      "VPN_AUTH_METHOD",
      connection.auth_method.as_deref().unwrap_or_default(),
    ),
    (
      "TARGET_IP",
      connection.target_ip.as_deref().unwrap_or_default(),
    ),
  ] {
    config.push_str(key);
    config.push('=');
    config.push_str(value);
    config.push('\n');
  }
  Ok(config)
}

fn find_engine() -> io::Result<PathBuf> {
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

fn prepare_config(path: &Path) -> io::Result<String> {
  let path = path.canonicalize()?;
  let metadata = path.metadata()?;
  if !metadata.is_file() {
    return Err(io::Error::other("VPN env file must be a regular file"));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o077 != 0 {
      return Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "VPN env file must be private; run chmod 600 on the file",
      ));
    }
  }
  let path = path
    .to_str()
    .filter(|path| !path.contains([',', '\n', '\r']))
    .ok_or_else(|| io::Error::other("VPN env path must be UTF-8 without commas or newlines"))?;
  Ok(path.to_owned())
}

#[derive(Deserialize)]
struct PortBinding {
  #[serde(rename = "HostIp")]
  host_ip: String,
  #[serde(rename = "HostPort")]
  host_port: String,
}

fn parse_published_port(bytes: &[u8]) -> io::Result<u16> {
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
