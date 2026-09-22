//! Resolve OpenSSH's sharing policy without taking ownership of its sockets.

use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use ctld_ipc::SshTarget;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;

use super::{
  MasterEndpoint, RequestError, SSH_PROGRAM, SharedMasterStartup, append_target_arguments,
  control_master_is_ready,
};

const CONFIG_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONFIG_BYTES: u64 = 256 * 1024;

#[derive(Debug, PartialEq, Eq)]
struct SharingPolicy {
  path: Option<PathBuf>,
  can_create: bool,
  asks_permission: bool,
}

/// `ssh -G` resolves Host/Match/Include, the account, and `ControlPath` tokens.
/// Keep connection options identical to shared startup, including session type.
fn config_command(target: &SshTarget) -> Command {
  let mut command = Command::new(SSH_PROGRAM);
  command.arg("-G");
  append_session_options(&mut command);
  append_target_arguments(&mut command, target);
  command.arg(SHARED_SESSION_COMMAND);
  command
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  command
}

pub(super) const SHARED_SESSION_COMMAND: &str = "printf 'ctld-master-ready\\n'; exec cat";

pub(super) fn append_session_options(command: &mut Command) {
  command
    .arg("-T")
    .args(["-o", "ForkAfterAuthentication=no"])
    .args(["-o", "StdinNull=no"])
    .args(["-o", "SessionType=default"])
    .args(["-o", "ClearAllForwardings=yes"])
    .args(["-o", "ForwardAgent=no"])
    .args(["-o", "ForwardX11=no"])
    .args(["-o", "PermitLocalCommand=no"])
    .args(["-o", "RemoteCommand=none"])
    .args(["-o", "BatchMode=no"])
    .args(["-o", "StrictHostKeyChecking=ask"]);
}

pub(super) async fn resolve(target: &SshTarget) -> Result<MasterEndpoint, RequestError> {
  if target.ssh_config_alias.is_none() {
    return Ok(MasterEndpoint::managed(target));
  }
  let policy = read_policy(config_command(target)).await?;
  if let Some(path) = policy.path {
    // ControlMaster=no can still use a running master; it only disables
    // creation. A missing path or disabled creation uses rmux's private master.
    let ready = control_master_is_ready(target, &path).await;
    if policy.asks_permission && !ready {
      return Err(external_master_required());
    }
    if policy.can_create || ready {
      return Ok(MasterEndpoint {
        control_path: path,
        shared: true,
        startup: if policy.asks_permission {
          SharedMasterStartup::ExternalOnly
        } else if policy.can_create {
          SharedMasterStartup::Create
        } else {
          SharedMasterStartup::PrivateFallback
        },
      });
    }
  }
  Ok(MasterEndpoint::managed(target))
}

pub(super) fn external_master_required() -> RequestError {
  RequestError::SshConfig(
    "ControlMaster ask/autoask requires a master started outside rmux so OpenSSH can keep showing its sharing confirmations. Start this SSH alias in your terminal, then connect again.".into(),
  )
}

async fn read_policy(mut command: Command) -> Result<SharingPolicy, RequestError> {
  let mut child = command.spawn().map_err(RequestError::StartMaster)?;
  let stdout = child
    .stdout
    .take()
    .ok_or(RequestError::InvalidRequest("SSH config stdout missing"))?;
  let stderr = child
    .stderr
    .take()
    .ok_or(RequestError::InvalidRequest("SSH config stderr missing"))?;
  let result = tokio::time::timeout(CONFIG_TIMEOUT, async {
    let (status, stdout, stderr) =
      tokio::try_join!(child.wait(), read_limited(stdout), read_limited(stderr))?;
    if !status.success() {
      return Err(io::Error::other(format!(
        "could not evaluate SSH config: {}",
        String::from_utf8_lossy(&stderr).trim()
      )));
    }
    parse_policy(&stdout)
  })
  .await;
  match result {
    Ok(Ok(policy)) => Ok(policy),
    Ok(Err(error)) => {
      let _ = child.kill().await;
      Err(RequestError::SshConfig(error.to_string()))
    }
    Err(_) => {
      let _ = child.kill().await;
      Err(RequestError::SshConfig(
        "OpenSSH did not finish evaluating its configuration".into(),
      ))
    }
  }
}

async fn read_limited(reader: impl AsyncRead + Unpin) -> io::Result<Vec<u8>> {
  let mut bytes = Vec::new();
  reader
    .take(MAX_CONFIG_BYTES + 1)
    .read_to_end(&mut bytes)
    .await?;
  if bytes.len() as u64 > MAX_CONFIG_BYTES {
    return Err(io::Error::other("SSH configuration output is too large"));
  }
  Ok(bytes)
}

fn parse_policy(output: &[u8]) -> io::Result<SharingPolicy> {
  let output = std::str::from_utf8(output).map_err(io::Error::other)?;
  let mut master = None;
  let mut path = None;
  for line in output.lines() {
    let Some((key, value)) = line.split_once(char::is_whitespace) else {
      continue;
    };
    let value = value.trim();
    match key {
      "controlmaster" => master = Some(value),
      "controlpath" if value != "none" => path = Some(PathBuf::from(value)),
      _ => {}
    }
  }
  let can_create = match master {
    Some("true" | "yes" | "auto" | "ask" | "autoask") => true,
    Some("false" | "no") => false,
    _ => {
      return Err(io::Error::other(
        "OpenSSH returned an unknown ControlMaster policy",
      ));
    }
  };
  if let Some(value) = path.as_mut() {
    if value.as_os_str().is_empty() {
      return Err(io::Error::other("OpenSSH returned an empty ControlPath"));
    }
    if !value.is_absolute() {
      *value = std::env::current_dir()?.join(&*value);
    }
  }
  Ok(SharingPolicy {
    path,
    can_create,
    asks_permission: matches!(master, Some("ask" | "autoask")),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_enabled_disabled_and_expanded_paths_without_reimplementing_tokens() {
    for value in ["true", "yes", "auto", "ask", "autoask"] {
      let text = format!(
        "controlmaster {value}\ncontrolpath /tmp/SSH sockets/user@host:22\ncontrolpersist 0\n"
      );
      assert_eq!(
        parse_policy(text.as_bytes()).unwrap(),
        SharingPolicy {
          path: Some(PathBuf::from("/tmp/SSH sockets/user@host:22")),
          can_create: true,
          asks_permission: matches!(value, "ask" | "autoask"),
        }
      );
    }
    for text in [
      "controlmaster false\ncontrolpath none\n",
      "controlmaster no\n",
    ] {
      assert_eq!(
        parse_policy(text.as_bytes()).unwrap(),
        SharingPolicy {
          path: None,
          can_create: false,
          asks_permission: false
        }
      );
    }
    assert!(
      !parse_policy(b"controlmaster no\ncontrolpath /tmp/master\n")
        .unwrap()
        .can_create
    );
    assert_eq!(
      parse_policy(b"controlmaster auto\ncontrolpath relative/socket\n")
        .unwrap()
        .path,
      Some(std::env::current_dir().unwrap().join("relative/socket"))
    );
    assert!(parse_policy(b"controlmaster invalid\n").is_err());
  }

  #[tokio::test]
  async fn bounds_config_output() {
    assert!(
      read_limited(&vec![b'x'; usize::try_from(MAX_CONFIG_BYTES).unwrap() + 1][..])
        .await
        .is_err()
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn openssh_resolves_includes_and_tokens_without_connecting() {
    let root = std::env::temp_dir().join(format!("ctld-config-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("included"), "Host fixture\n HostName 127.0.0.1\n User tester\n Port 2201\n ControlMaster auto\n ControlPath /tmp/test-mux-%r-%h-%p\n ControlPersist 2m\n").unwrap();
    std::fs::write(
      root.join("config"),
      format!("Include {}\n", root.join("included").display()),
    )
    .unwrap();
    let mut command = Command::new(SSH_PROGRAM);
    command
      .args(["-G", "-F"])
      .arg(root.join("config"))
      .arg("fixture")
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .kill_on_drop(true);
    let result = read_policy(command).await;
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(
      result.unwrap(),
      SharingPolicy {
        path: Some(PathBuf::from("/tmp/test-mux-tester-127.0.0.1-2201")),
        can_create: true,
        asks_permission: false,
      }
    );
  }
}
