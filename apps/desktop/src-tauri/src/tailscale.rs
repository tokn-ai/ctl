//! Read-only discovery through the user's installed Tailscale client.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;

#[cfg(test)]
mod tests;

const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STDOUT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_STDERR_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TailscaleDevice {
  pub node_id: String,
  pub name: String,
  pub dns_name: Option<String>,
  pub addresses: Vec<String>,
  pub online: Option<bool>,
  pub os: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryState {
  Available,
  NotInstalled,
  NotRunning,
  NeedsLogin,
  Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TailscaleDiscovery {
  pub devices: Vec<TailscaleDevice>,
  pub warnings: Vec<String>,
  pub state: DiscoveryState,
}

impl TailscaleDiscovery {
  fn unavailable(state: DiscoveryState, warning: &str) -> Self {
    Self {
      devices: Vec::new(),
      warnings: vec![warning.into()],
      state,
    }
  }
}

/// Discovery never starts, logs in, or reconfigures Tailscale.
#[tauri::command]
pub async fn list_tailscale_devices() -> TailscaleDiscovery {
  let candidates = cli_candidates(
    std::env::var_os("PATH").as_deref(),
    &platform_candidates(dirs::home_dir().as_deref()),
  );
  discover(&candidates, STATUS_TIMEOUT).await
}

fn cli_candidates(search_path: Option<&OsStr>, fallback_paths: &[PathBuf]) -> Vec<PathBuf> {
  let name = if cfg!(windows) {
    "tailscale.exe"
  } else {
    "tailscale"
  };
  let mut seen = HashSet::new();
  search_path
    .into_iter()
    .flat_map(std::env::split_paths)
    // A GUI app must not execute an unrelated file from its working directory.
    .filter(|directory| directory.is_absolute())
    .map(|directory| directory.join(name))
    .chain(fallback_paths.iter().cloned())
    .filter(|candidate| seen.insert(candidate.clone()))
    .collect()
}

fn platform_candidates(home: Option<&Path>) -> Vec<PathBuf> {
  #[cfg(not(target_os = "macos"))]
  let _ = home;
  #[cfg(target_os = "macos")]
  {
    let mut paths = mac_app_candidates(Path::new("/Applications"));
    if let Some(home) = home {
      paths.extend(mac_app_candidates(&home.join("Applications")));
    }
    paths.extend([
      PathBuf::from("/opt/homebrew/bin/tailscale"),
      PathBuf::from("/usr/local/bin/tailscale"),
    ]);
    paths
  }
  #[cfg(windows)]
  {
    std::env::var_os("ProgramFiles")
      .map(|directory| PathBuf::from(directory).join("Tailscale/tailscale.exe"))
      .into_iter()
      .collect()
  }
  #[cfg(not(any(target_os = "macos", windows)))]
  {
    Vec::new()
  }
}

#[cfg(any(target_os = "macos", test))]
fn mac_app_candidates(applications: &Path) -> Vec<PathBuf> {
  let directory = applications.join("Tailscale.app/Contents/MacOS");
  vec![directory.join("Tailscale"), directory.join("tailscale")]
}

async fn discover(candidates: &[PathBuf], timeout: Duration) -> TailscaleDiscovery {
  let mut spawn_error = None;
  for executable in candidates {
    match read_status(executable, timeout).await {
      Ok(output) => return discovery_from_output(&output),
      Err(error) if error.kind() == io::ErrorKind::NotFound => {}
      Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
        spawn_error = Some(error);
      }
      Err(error) => {
        return TailscaleDiscovery::unavailable(
          DiscoveryState::Error,
          &format!("Could not read Tailscale devices: {error}"),
        );
      }
    }
  }
  if let Some(error) = spawn_error {
    return TailscaleDiscovery::unavailable(
      DiscoveryState::Error,
      &format!("Could not run the installed Tailscale client: {error}"),
    );
  }
  TailscaleDiscovery::unavailable(
    DiscoveryState::NotInstalled,
    "Tailscale was not found. Install the Tailscale client to discover devices.",
  )
}

struct StatusOutput {
  success: bool,
  stdout: Vec<u8>,
  stderr: Vec<u8>,
}

async fn read_status(executable: &Path, timeout: Duration) -> io::Result<StatusOutput> {
  let mut child = Command::new(executable)
    .args(["status", "--json", "--peers=true"])
    // The app executable also serves as the CLI. Without this variable a GUI
    // process may open another Tailscale window instead of reading status.
    // https://tailscale.com/docs/reference/tailscale-cli?tab=macos
    .env("TAILSCALE_BE_CLI", "1")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()?;
  let stdout = child
    .stdout
    .take()
    .ok_or_else(|| io::Error::other("Tailscale stdout was not captured"))?;
  let stderr = child
    .stderr
    .take()
    .ok_or_else(|| io::Error::other("Tailscale stderr was not captured"))?;
  let result = tokio::time::timeout(timeout, async {
    let (status, stdout, stderr) = tokio::try_join!(
      child.wait(),
      read_limited(stdout, MAX_STDOUT_BYTES),
      read_limited(stderr, MAX_STDERR_BYTES),
    )?;
    Ok(StatusOutput {
      success: status.success(),
      stdout,
      stderr,
    })
  })
  .await
  .unwrap_or_else(|_| {
    Err(io::Error::new(
      io::ErrorKind::TimedOut,
      "Tailscale did not respond in time. Check that its client is running.",
    ))
  });
  if result.is_err() {
    let _ignored = child.kill().await;
  }
  result
}

async fn read_limited(reader: impl AsyncRead + Unpin, limit: u64) -> io::Result<Vec<u8>> {
  let mut bytes = Vec::new();
  reader.take(limit + 1).read_to_end(&mut bytes).await?;
  if bytes.len() as u64 > limit {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "Tailscale returned too much status output",
    ));
  }
  Ok(bytes)
}

fn discovery_from_output(output: &StatusOutput) -> TailscaleDiscovery {
  // Some client versions exit unsuccessfully while still reporting a useful
  // JSON BackendState such as NeedsLogin or Stopped.
  if let Ok(status) = serde_json::from_slice::<Value>(&output.stdout) {
    let discovery = parse_status(&status);
    if output.success || discovery.state != DiscoveryState::Available {
      return discovery;
    }
  } else if output.success {
    return TailscaleDiscovery::unavailable(
      DiscoveryState::Error,
      "Tailscale returned invalid status data. Update or restart its client and refresh.",
    );
  }
  let error = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
  if error.contains("logged out") || error.contains("not logged in") {
    return TailscaleDiscovery::unavailable(
      DiscoveryState::NeedsLogin,
      "Sign in to Tailscale, then refresh the device list.",
    );
  }
  let state = if error.contains("failed to connect to local tailscaled")
    || error.contains("tailscale is not running")
    || error.contains("tailscaled is not running")
  {
    DiscoveryState::NotRunning
  } else {
    DiscoveryState::Error
  };
  TailscaleDiscovery::unavailable(
    state,
    "Could not read Tailscale devices. Check that its client is running and signed in, then refresh.",
  )
}

fn parse_status(status: &Value) -> TailscaleDiscovery {
  match status.get("BackendState").and_then(Value::as_str) {
    Some("Running") => {}
    Some("NeedsLogin" | "NeedsMachineAuth") => {
      return TailscaleDiscovery::unavailable(
        DiscoveryState::NeedsLogin,
        "Sign in or approve this device in Tailscale, then refresh the device list.",
      );
    }
    Some("Stopped" | "Starting" | "NoState") => {
      return TailscaleDiscovery::unavailable(
        DiscoveryState::NotRunning,
        "Connect Tailscale, then refresh the device list.",
      );
    }
    _ => {
      return TailscaleDiscovery::unavailable(
        DiscoveryState::Error,
        "Tailscale returned an unknown client state. Update or restart its client and refresh.",
      );
    }
  }
  let mut discovery = TailscaleDiscovery {
    devices: Vec::new(),
    warnings: Vec::new(),
    state: DiscoveryState::Available,
  };
  let Some(peers) = status.get("Peer").filter(|value| !value.is_null()) else {
    return discovery;
  };
  let Some(peers) = peers.as_object() else {
    return TailscaleDiscovery::unavailable(
      DiscoveryState::Error,
      "Tailscale returned an invalid device list. Update or restart its client and refresh.",
    );
  };
  let self_id = status.pointer("/Self/ID").and_then(Value::as_str);
  let mut seen = HashSet::new();
  let mut invalid = false;
  for peer in peers.values() {
    // ShareeNode is an inbound-only sharing recipient, not a shared-in device.
    // Match the CLI's filtering: tailscale/ipn/ipnstate/ipnstate.go PeerStatus.
    if peer.get("ID").and_then(Value::as_str) == self_id && self_id.is_some()
      || peer.get("ShareeNode").and_then(Value::as_bool) == Some(true)
      || peer.get("Expired").and_then(Value::as_bool) == Some(true)
      || peer.get("InNetworkMap").and_then(Value::as_bool) == Some(false)
    {
      continue;
    }
    if let Some(device) = parse_peer(peer) {
      if seen.insert(device.node_id.clone()) {
        discovery.devices.push(device);
      }
    } else {
      invalid = true;
    }
  }
  if invalid {
    discovery.warnings.push(
      "Some Tailscale devices were skipped because their identity or address was invalid.".into(),
    );
  }
  discovery.devices.sort_by(|left, right| {
    left
      .name
      .to_lowercase()
      .cmp(&right.name.to_lowercase())
      .then_with(|| left.node_id.cmp(&right.node_id))
  });
  discovery
}

pub(crate) fn valid_node_id(value: &str) -> bool {
  !value.is_empty()
    && value.len() <= 256
    && value
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_display_text(value: &str) -> bool {
  !value.trim().is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
}

fn valid_dns_name(value: &str) -> bool {
  value.len() <= 253
    && value.split('.').all(|label| {
      !label.is_empty()
        && label.len() <= 63
        && label.as_bytes()[0].is_ascii_alphanumeric()
        && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
        && label
          .bytes()
          .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn parse_peer(peer: &Value) -> Option<TailscaleDevice> {
  let node_id = peer.get("ID")?.as_str().filter(|id| valid_node_id(id))?;
  let mut addresses: Vec<String> = peer
    .get("TailscaleIPs")
    .and_then(Value::as_array)
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .filter_map(|address| address.parse::<IpAddr>().ok())
    .filter(|address| {
      !address.is_unspecified() && !address.is_loopback() && !address.is_multicast()
    })
    .map(|address| address.to_string())
    .collect();
  addresses.dedup();
  // Requiring an address avoids inventing a route from a display-only hostname.
  let address = addresses.first()?;
  let dns_name = peer
    .get("DNSName")
    .and_then(Value::as_str)
    .map(|name| name.trim_end_matches('.'))
    .filter(|name| valid_dns_name(name))
    .map(str::to_owned);
  let name = peer
    .get("HostName")
    .and_then(Value::as_str)
    .filter(|name| valid_display_text(name))
    .map(str::to_owned)
    .or_else(|| dns_name.clone())
    .unwrap_or_else(|| address.clone());
  Some(TailscaleDevice {
    node_id: node_id.into(),
    name,
    dns_name,
    addresses,
    online: peer.get("Online").and_then(Value::as_bool),
    os: peer
      .get("OS")
      .and_then(Value::as_str)
      .filter(|os| valid_display_text(os))
      .map(str::to_owned),
  })
}
