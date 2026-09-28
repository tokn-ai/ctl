//! A container lease owned by ctld, including when the host process crashes.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use ctld_ipc::VpnStatus;
use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep, timeout};

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
}

impl ManagedVpn {
  #[must_use]
  pub fn status(&self) -> VpnStatus {
    VpnStatus {
      endpoint: self.endpoint.clone(),
      container_name: Some(self.container_name.clone()),
      running: self.endpoint.is_some(),
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
    loop {
      if let Some(status) = self.child.try_wait()? {
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
  start_with_engine(options, Path::new("docker")).await
}

async fn start_with_engine(options: Options, engine: &Path) -> io::Result<ManagedVpn> {
  let config = prepare_config(&options.env_file)?;
  let container_name = format!("ctld-openconnect-{}", uuid::Uuid::new_v4().simple());
  let mut child = engine_command(engine)
    .args([
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
      "--mount",
      &format!("type=bind,source={config},target=/run/secrets/openconnect.env,readonly"),
      IMAGE,
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
  let mut vpn = ManagedVpn {
    child,
    heartbeat,
    container_name,
    endpoint: None,
    engine: engine.to_path_buf(),
  };
  let ready = timeout(START_TIMEOUT, vpn.wait_ready())
    .await
    .unwrap_or_else(|_| {
      Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "OpenConnect VPN and SOCKS5 listener did not become ready within 75 seconds",
      ))
    });
  if let Err(error) = ready {
    vpn.shutdown().await;
    return Err(error);
  }
  Ok(vpn)
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
