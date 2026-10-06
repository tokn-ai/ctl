//! Bind reconnect approval to effective SSH settings and saved server trust.

use ctl_ipc::{GatewayKind, SshTarget};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;

const MAX_CONFIG_BYTES: usize = 256 * 1024;
const MAX_KNOWN_HOSTS_BYTES: usize = 4 * 1024 * 1024;
const MAX_PATH_TOKENS: usize = 16;
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// Failure disables authorization reuse without preventing ordinary connection.
pub(crate) async fn snapshot(target: &SshTarget) -> Option<String> {
  tokio::time::timeout(SNAPSHOT_TIMEOUT, snapshot_inner(target))
    .await
    .ok()
    .flatten()
}

async fn snapshot_inner(target: &SshTarget) -> Option<String> {
  crate::validate_target(target).ok()?;
  let target_bytes = serde_json::to_vec(target).ok()?;
  if target_bytes.len() > MAX_CONFIG_BYTES {
    return None;
  }
  let mut hash = Sha256::new();
  hash_part(&mut hash, b"ctl-reconnect-approval-scope-v1");
  hash_part(&mut hash, &target_bytes);
  let mut remaining = MAX_KNOWN_HOSTS_BYTES;
  let mut files = HashMap::new();
  for current in route_targets(target) {
    let config = effective_config(&current).await?;
    let known_hosts = known_host_paths(&config, !current.gateways.is_empty())?;
    hash_part(&mut hash, &config);
    for path in known_hosts {
      hash_part(&mut hash, path.as_os_str().as_encoded_bytes());
      let fingerprint = if let Some(fingerprint) = files.get(&path) {
        *fingerprint
      } else {
        let fingerprint = known_host_fingerprint(&path, &mut remaining).await?;
        files.insert(path, fingerprint);
        fingerprint
      };
      hash_part(&mut hash, &fingerprint);
    }
  }
  Some(format!("{:x}", hash.finalize()))
}

fn route_targets(target: &SshTarget) -> Vec<SshTarget> {
  let mut targets = vec![target.clone()];
  targets.extend(
    target
      .gateways
      .iter()
      .enumerate()
      .filter(|(_, gateway)| gateway.kind == GatewayKind::Ssh)
      .map(|(index, gateway)| SshTarget {
        destination: gateway.destination.clone(),
        ssh_config_alias: gateway
          .hostname
          .as_ref()
          .map(|_| gateway.destination.clone()),
        use_ssh_config_master: Some(false),
        hostname: gateway.hostname.clone(),
        user: gateway.user.clone(),
        port: gateway.port,
        identity_file: gateway.identity_file.clone(),
        gateways: target.gateways[..index].to_vec(),
      }),
  );
  targets
}

async fn effective_config(target: &SshTarget) -> Option<Vec<u8>> {
  let mut command = Command::new(crate::SSH_PROGRAM);
  command
    .arg("-G")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
  crate::append_target_arguments(&mut command, target);
  let mut child = command.spawn().ok()?;
  let mut output = Vec::new();
  child
    .stdout
    .take()?
    .take((MAX_CONFIG_BYTES + 1) as u64)
    .read_to_end(&mut output)
    .await
    .ok()?;
  if output.len() > MAX_CONFIG_BYTES || !child.wait().await.ok()?.success() {
    return None;
  }
  Some(output)
}

fn known_host_paths(config: &[u8], explicit_route: bool) -> Option<Vec<PathBuf>> {
  let config = std::str::from_utf8(config).ok()?;
  let mut paths = Vec::new();
  let mut user_paths = false;
  let mut global_paths = false;
  for line in config.lines() {
    let (name, value) = line.split_once(' ')?;
    match name {
      "knownhostscommand" if value.trim() != "none" => return None,
      "proxyjump" if !explicit_route && value.trim() != "none" => return None,
      "verifyhostkeydns" if !matches!(value.trim(), "no" | "false") => return None,
      "nohostauthenticationforlocalhost" if !matches!(value.trim(), "no" | "false") => return None,
      "stricthostkeychecking" if !matches!(value.trim(), "yes" | "true" | "ask") => {
        return None;
      }
      "revokedhostkeys" => paths.extend(possible_paths(value)?),
      "userknownhostsfile" | "globalknownhostsfile" => {
        if name == "userknownhostsfile" {
          user_paths = true;
        } else {
          global_paths = true;
        }
        if value.is_empty() {
          return None;
        }
        paths.extend(possible_paths(value)?);
      }
      _ => {}
    }
  }
  (user_paths && global_paths).then_some(paths)
}

fn possible_paths(value: &str) -> Option<Vec<PathBuf>> {
  if value == "none" {
    return Some(Vec::new());
  }
  if value != value.trim() || value.contains("  ") || value.chars().any(char::is_control) {
    return None;
  }
  let mut tokens = Vec::new();
  let mut offset = 0;
  for token in value.split_whitespace() {
    if token == "none" || tokens.len() == MAX_PATH_TOKENS {
      return None;
    }
    let start = offset + value[offset..].find(token)?;
    offset = start + token.len();
    tokens.push((start, offset));
  }
  if tokens.is_empty() {
    return None;
  }
  // OpenSSH's config dump does not quote filename arrays. Include every bounded
  // contiguous group as a possible pathname, preserving the actual whitespace,
  // so a configured filename containing spaces cannot hide a trust-file change.
  let mut paths = Vec::new();
  for start in 0..tokens.len() {
    for end in start..tokens.len() {
      paths.push(resolve_path(&value[tokens[start].0..tokens[end].1])?);
    }
  }
  Some(paths)
}

fn resolve_path(value: &str) -> Option<PathBuf> {
  // OpenSSH expands some path tokens after -G. Cache only the paths whose actual
  // meaning we can establish; do not guess user/host substitutions or commands.
  if value.is_empty()
    || value.chars().any(|character| {
      matches!(
        character,
        '%' | '$' | '*' | '?' | '[' | ']' | '\\' | '\'' | '"'
      ) || character.is_control()
    })
  {
    return None;
  }
  let path = if let Some(relative) = value.strip_prefix("~/") {
    dirs::home_dir()?.join(relative)
  } else if value.starts_with('~') {
    return None;
  } else {
    PathBuf::from(value)
  };
  Some(if path.is_absolute() {
    path
  } else {
    std::env::current_dir().ok()?.join(path)
  })
}

async fn known_host_fingerprint(path: &Path, remaining: &mut usize) -> Option<[u8; 32]> {
  let mut hash = Sha256::new();
  let metadata = match tokio::fs::metadata(path).await {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      // Absent default system files are normal. Hash absence so creation or
      // deletion changes the approval scope just as a content change does.
      hash_part(&mut hash, b"missing");
      return Some(hash.finalize().into());
    }
    Err(_) => return None,
  };
  if !metadata.is_file() || metadata.len() > u64::try_from(*remaining).ok()? {
    return None;
  }
  let file = tokio::fs::File::open(path).await.ok()?;
  let mut contents = Vec::new();
  file
    .take(u64::try_from(*remaining).ok()?.checked_add(1)?)
    .read_to_end(&mut contents)
    .await
    .ok()?;
  *remaining = remaining.checked_sub(contents.len())?;
  hash_part(&mut hash, b"present");
  hash_part(&mut hash, &contents);
  Some(hash.finalize().into())
}

fn hash_part(hash: &mut Sha256, data: &[u8]) {
  hash.update((data.len() as u64).to_be_bytes());
  hash.update(data);
}

#[cfg(test)]
mod tests {
  use super::*;

  fn config(extra: &str) -> Vec<u8> {
    format!("hostname fixture.invalid\nuser alice\nuserknownhostsfile /fixture/user_known_hosts\nglobalknownhostsfile /fixture/global_known_hosts /fixture/global_known_hosts2\n{extra}").into_bytes()
  }

  #[test]
  fn unknown_dynamic_trust_or_implicit_jump_disables_reuse() {
    for extra in [
      "knownhostscommand /bin/echo trusted\n",
      "proxyjump private-gateway\n",
      "verifyhostkeydns yes\n",
      "verifyhostkeydns ask\n",
      "nohostauthenticationforlocalhost yes\n",
      "stricthostkeychecking no\n",
      "stricthostkeychecking accept-new\n",
    ] {
      assert!(known_host_paths(&config(extra), false).is_none());
    }
    assert!(known_host_paths(&config("knownhostscommand none\nproxyjump none\n"), false).is_some());
    assert!(known_host_paths(&config("proxyjump explicit-gateway\n"), true).is_some());
    assert!(known_host_paths(&config("stricthostkeychecking true\nverifyhostkeydns false\nnohostauthenticationforlocalhost no\n"), false).is_some());
    assert!(known_host_paths(b"userknownhostsfile /fixture\n", false).is_none());
    assert!(known_host_paths(b"userknownhostsfile \xff\n", false).is_none());
    let revoked = known_host_paths(&config("revokedhostkeys /fixture/revoked\n"), false).unwrap();
    assert!(revoked.contains(&PathBuf::from("/fixture/revoked")));
  }

  #[test]
  fn accepting_new_servers_never_reuses_approval_even_without_persistent_trust() {
    let config =
      b"stricthostkeychecking accept-new\nuserknownhostsfile none\nglobalknownhostsfile none\n";
    assert!(known_host_paths(config, false).is_none());
    assert!(known_host_paths(config, true).is_none());
  }

  #[test]
  fn resolves_only_understood_known_host_paths() {
    let paths = known_host_paths(&config(""), false).unwrap();
    assert_eq!(
      paths,
      [
        PathBuf::from("/fixture/user_known_hosts"),
        PathBuf::from("/fixture/global_known_hosts"),
        PathBuf::from("/fixture/global_known_hosts /fixture/global_known_hosts2"),
        PathBuf::from("/fixture/global_known_hosts2")
      ]
    );
    for path in [
      "~other/.ssh/known_hosts",
      "/fixture/%h",
      "/fixture/${HOME}",
      "/fixture/*",
      "/fixture/[a]",
      "\"/fixture path\"",
      "",
    ] {
      assert!(resolve_path(path).is_none(), "unsupported path: {path}");
    }
  }

  #[test]
  fn unquoted_path_lists_include_every_possible_space_containing_filename() {
    let candidates = possible_paths("/fixture/known hosts /fixture/other").unwrap();
    assert!(candidates.contains(&PathBuf::from("/fixture/known hosts")));
    assert!(candidates.contains(&PathBuf::from("/fixture/known hosts /fixture/other")));
    assert!(candidates.contains(&PathBuf::from("/fixture/other")));
    assert_eq!(possible_paths("none").unwrap(), Vec::<PathBuf>::new());
    assert!(possible_paths("/fixture/known\thosts").is_none());
    assert!(possible_paths("/fixture/known  hosts").is_none());
    assert!(possible_paths("/fixture/known ").is_none());
    assert!(possible_paths(" /fixture/known").is_none());
    assert!(possible_paths(&["/fixture"; MAX_PATH_TOKENS + 1].join(" ")).is_none());
  }

  #[test]
  fn snapshot_inspects_each_ssh_hop_with_its_actual_route_prefix() {
    use ctl_ipc::{SshGateway, SshGatewayMode};
    let gateway = |name: &str, kind| SshGateway {
      kind,
      vpn: None,
      destination: name.into(),
      hostname: Some(format!("{name}.invalid")),
      user: Some("gateway-user".into()),
      port: Some(2222),
      identity_file: None,
      mode: SshGatewayMode::Automatic,
    };
    let target = SshTarget {
      destination: "destination".into(),
      ssh_config_alias: None,
      use_ssh_config_master: Some(false),
      hostname: None,
      user: Some("destination-user".into()),
      port: None,
      identity_file: None,
      gateways: vec![
        gateway("first", GatewayKind::Ssh),
        gateway("proxy", GatewayKind::Socks5),
        gateway("second", GatewayKind::Ssh),
      ],
    };
    let route = route_targets(&target);
    assert_eq!(route.len(), 3);
    assert_eq!(route[0], target);
    assert_eq!(route[1].destination, "first");
    assert_eq!(route[1].ssh_config_alias.as_deref(), Some("first"));
    assert_eq!(route[1].gateways, Vec::<SshGateway>::new());
    assert_eq!(route[2].destination, "second");
    assert_eq!(route[2].gateways, target.gateways[..2]);
    assert_eq!(route[2].user.as_deref(), Some("gateway-user"));
    assert_eq!(route[2].hostname.as_deref(), Some("second.invalid"));
  }

  struct Fixture(PathBuf);
  impl Fixture {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("ctl-approval-scope-{}", uuid::Uuid::new_v4()));
      std::fs::create_dir(&path).unwrap();
      Self(path)
    }
  }
  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  #[tokio::test]
  async fn trust_creation_update_and_removal_change_the_scope() {
    async fn fingerprint(path: &Path) -> [u8; 32] {
      let mut remaining = MAX_KNOWN_HOSTS_BYTES;
      known_host_fingerprint(path, &mut remaining).await.unwrap()
    }
    let fixture = Fixture::new();
    let path = fixture.0.join("known_hosts");
    let missing = fingerprint(&path).await;
    std::fs::write(&path, b"fixture ssh-ed25519 first\n").unwrap();
    let first = fingerprint(&path).await;
    std::fs::write(&path, b"fixture ssh-ed25519 second\n").unwrap();
    let second = fingerprint(&path).await;
    assert_ne!(missing, first);
    assert_ne!(first, second);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(missing, fingerprint(&path).await);
  }

  #[tokio::test]
  async fn trust_files_share_a_strict_read_budget_and_require_regular_files() {
    let fixture = Fixture::new();
    let path = fixture.0.join("known_hosts");
    std::fs::write(&path, b"1234").unwrap();
    let mut remaining = 5;
    assert!(
      known_host_fingerprint(&path, &mut remaining)
        .await
        .is_some()
    );
    assert_eq!(remaining, 1);
    assert!(
      known_host_fingerprint(&path, &mut remaining)
        .await
        .is_none()
    );
    assert!(
      known_host_fingerprint(&fixture.0, &mut remaining)
        .await
        .is_none()
    );
  }

  #[test]
  fn hash_parts_have_unambiguous_boundaries() {
    let mut first = Sha256::new();
    let mut second = Sha256::new();
    for part in [b"a".as_slice(), b"bc"] {
      hash_part(&mut first, part);
    }
    for part in [b"ab".as_slice(), b"c"] {
      hash_part(&mut second, part);
    }
    assert_ne!(first.finalize(), second.finalize());
  }
}
