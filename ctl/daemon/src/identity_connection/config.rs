//! Bounded local OpenSSH configuration discovery; never derive paths from prompts.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use ctl_ipc::{GatewayKind, SshGateway, SshTarget};
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;

use super::MAX_IDENTITIES;

const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const CONFIG_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_GATEWAYS: usize = 8;

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) enum PublicKeyAuthentication {
  #[default]
  Enabled,
  Disabled,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Configuration {
  pub(super) paths: Vec<String>,
  pub(super) identity_files: Vec<String>,
  pub(super) resolved_identity_files: Vec<(String, String)>,
  pub(super) agent: Option<PathBuf>,
  pub(super) agent_disabled: bool,
  pub(super) publickey_authentication: PublicKeyAuthentication,
  pub(super) mutates_agent: bool,
  pub(super) inherits_agent: bool,
  pub(super) jumps: Vec<SshTarget>,
}

pub(super) struct Configurations {
  pub(super) destination: Configuration,
  pub(super) gateways: Vec<Configuration>,
}

pub(super) async fn resolve_configurations(target: &SshTarget) -> std::io::Result<Configurations> {
  resolve_with(
    target,
    |target| async move { read_configuration(&target).await },
  )
  .await
}

async fn resolve_with<F: Future<Output = std::io::Result<Configuration>>>(
  target: &SshTarget,
  mut read: impl FnMut(SshTarget) -> F,
) -> std::io::Result<Configurations> {
  let destination = read(target.clone()).await?;
  let mut queue: VecDeque<_> = target
    .gateways
    .iter()
    .filter_map(gateway_target)
    .take(MAX_GATEWAYS)
    .collect();
  queue.extend(destination.jumps.iter().cloned());
  let mut seen = HashSet::new();
  seen.insert(target_identity(target));
  let mut gateways = Vec::new();
  let deadline = tokio::time::Instant::now() + CONFIG_TIMEOUT;
  while let Some(gateway) = queue.pop_front() {
    if seen.len() > MAX_GATEWAYS {
      break;
    }
    if !seen.insert(target_identity(&gateway)) {
      continue;
    }
    let Ok(Ok(configuration)) = tokio::time::timeout_at(deadline, read(gateway)).await else {
      continue;
    };
    for jump in configuration.jumps.iter().take(MAX_GATEWAYS) {
      if queue.len() < MAX_GATEWAYS {
        queue.push_back(jump.clone());
      }
    }
    gateways.push(configuration);
  }
  Ok(Configurations {
    destination,
    gateways,
  })
}

fn target_identity(target: &SshTarget) -> (String, Option<String>, Option<u16>) {
  (
    target
      .hostname
      .as_ref()
      .unwrap_or(&target.destination)
      .clone(),
    target.user.clone(),
    target.port,
  )
}

fn gateway_target(gateway: &SshGateway) -> Option<SshTarget> {
  (gateway.kind == GatewayKind::Ssh).then(|| SshTarget {
    destination: gateway
      .hostname
      .as_ref()
      .unwrap_or(&gateway.destination)
      .clone(),
    hostname: None,
    ssh_config_alias: None,
    use_ssh_config_master: None,
    user: gateway.user.clone(),
    port: gateway.port,
    identity_file: None,
    gateways: Vec::new(),
  })
}

fn jump_target(specification: &str) -> Option<SshTarget> {
  if specification.is_empty()
    || specification == "none"
    || specification.contains('%')
    || specification
      .chars()
      .any(|ch| ch.is_control() || ch.is_whitespace())
  {
    return None;
  }
  let url = url::Url::parse(&format!(
    "ssh://{}",
    specification
      .strip_prefix("ssh://")
      .unwrap_or(specification)
  ))
  .ok()?;
  if url.password().is_some()
    || url.query().is_some()
    || url.fragment().is_some()
    || !matches!(url.path(), "" | "/")
  {
    return None;
  }
  let host = url.host_str()?;
  if host.starts_with('-') {
    return None;
  }
  Some(SshTarget {
    destination: host.to_owned(),
    hostname: None,
    ssh_config_alias: None,
    use_ssh_config_master: None,
    user: (!url.username().is_empty()).then(|| url.username().to_owned()),
    port: url.port(),
    identity_file: None,
    gateways: Vec::new(),
  })
}

async fn read_configuration(target: &SshTarget) -> std::io::Result<Configuration> {
  let mut command = Command::new(crate::SSH_PROGRAM);
  command.arg("-G");
  crate::append_target_arguments(&mut command, target);
  command
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
  let mut child = command.spawn()?;
  let stdout = child
    .stdout
    .take()
    .ok_or_else(|| std::io::Error::other("missing SSH configuration"))?;
  let result = tokio::time::timeout(CONFIG_TIMEOUT, async {
    let mut bytes = Vec::new();
    stdout
      .take(MAX_CONFIG_BYTES + 1)
      .read_to_end(&mut bytes)
      .await?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES || !child.wait().await?.success() {
      return Err(std::io::Error::other("unavailable SSH configuration"));
    }
    let text = std::str::from_utf8(&bytes).map_err(std::io::Error::other)?;
    Ok(parse_configuration(text))
  })
  .await;
  match result {
    Ok(result) => result,
    Err(_) => Err(std::io::Error::other("SSH configuration timed out")),
  }
}

pub(super) fn parse_configuration(text: &str) -> Configuration {
  let mut configuration = Configuration::default();
  let mut explicit_agent = false;
  let mut preferred_authentication_allows_publickey = true;
  let fields: Vec<_> = text
    .lines()
    .filter_map(|line| line.split_once(' '))
    .collect();
  let hostname = fields
    .iter()
    .find_map(|(key, value)| (*key == "hostname").then_some(*value));
  let user = fields
    .iter()
    .find_map(|(key, value)| (*key == "user").then_some(*value));
  for (key, value) in fields {
    if value.is_empty() || value.chars().any(char::is_control) {
      continue;
    }
    match key {
      "identityfile" if value != "none" => {
        configuration.identity_files.push(value.to_owned());
        if configuration.paths.len() < MAX_IDENTITIES
          && let Some(path) = expand_config_path(value, hostname, user)
        {
          configuration
            .resolved_identity_files
            .push((value.to_owned(), path.to_string_lossy().into_owned()));
          configuration
            .paths
            .push(path.to_string_lossy().into_owned());
        }
      }
      "identityagent" => {
        explicit_agent = true;
        configuration.agent_disabled = value == "none";
        configuration.agent = if matches!(value, "none" | "SSH_AUTH_SOCK") {
          None
        } else if let Some(variable) = value.strip_prefix('$') {
          std::env::var_os(variable).map(PathBuf::from)
        } else {
          expand_config_path(value, hostname, user)
        };
        if value == "SSH_AUTH_SOCK" {
          configuration.agent = std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from);
        } else if value != "none" && !value.starts_with('$') && configuration.agent.is_none() {
          // Unfamiliar tokens are resolved only by OpenSSH. Preserve that agent
          // instead of replacing it with a partial view of available keys.
          configuration.agent_disabled = true;
        }
      }
      "proxyjump" => {
        configuration.jumps = value
          .split(',')
          .take(MAX_GATEWAYS)
          .filter_map(jump_target)
          .collect();
      }
      "preferredauthentications" => {
        preferred_authentication_allows_publickey =
          value.split(',').any(|method| method == "publickey");
      }
      "addkeystoagent" => configuration.mutates_agent = !matches!(value, "no" | "false"),
      _ => {}
    }
  }
  // Authentication eligibility belongs to each destination. A password-only
  // destination can still inherit an agent needed by a public-key jump host.
  if !preferred_authentication_allows_publickey
    || text.lines().any(|line| {
      matches!(
        line,
        "pubkeyauthentication no" | "pubkeyauthentication false"
      )
    })
  {
    configuration.publickey_authentication = PublicKeyAuthentication::Disabled;
  }
  configuration.agent_disabled |= configuration.identity_files.len() > MAX_IDENTITIES;
  configuration.agent_disabled |= configuration.mutates_agent;
  configuration.inherits_agent = !explicit_agent
    || text.lines().any(|line| {
      matches!(
        line,
        "identityagent SSH_AUTH_SOCK" | "identityagent $SSH_AUTH_SOCK"
      )
    });
  if !configuration.agent_disabled && !explicit_agent {
    configuration.agent = std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from);
  }
  configuration
}

pub(super) fn expand_home(path: &str) -> Option<PathBuf> {
  if let Some(rest) = path.strip_prefix("~/") {
    return dirs::home_dir().map(|home| home.join(rest));
  }
  (!path.starts_with('~')).then(|| PathBuf::from(path))
}

fn expand_config_path(path: &str, hostname: Option<&str>, user: Option<&str>) -> Option<PathBuf> {
  let home = dirs::home_dir();
  let mut expanded = String::new();
  let mut characters = path.chars();
  while let Some(character) = characters.next() {
    if character != '%' {
      expanded.push(character);
      continue;
    }
    match characters.next()? {
      '%' => expanded.push('%'),
      'd' => expanded.push_str(home.as_ref()?.to_str()?),
      'h' => expanded.push_str(hostname?),
      'r' => expanded.push_str(user?),
      // Leave unfamiliar tokens to OpenSSH; never guess another key path.
      _ => return None,
    }
  }
  if expanded.contains("${") {
    return None;
  }
  let path = expand_home(&expanded)?;
  if path.is_absolute() {
    Some(path)
  } else {
    std::env::current_dir().ok().map(|cwd| cwd.join(path))
  }
}

#[cfg(test)]
mod tests;
