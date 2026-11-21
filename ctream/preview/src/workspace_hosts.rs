use ctream_client::transport::{AgentLaunch, SshCommand};
use serde::Deserialize;
use std::{io, path::PathBuf, process::Command};

#[derive(Clone)]
pub(super) struct Workspace {
  ctl: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Host {
  #[serde(rename = "host_id")]
  pub(super) id: String,
  pub(super) name: String,
}

#[derive(Deserialize)]
struct HostView {
  host: Host,
}

impl Default for Workspace {
  fn default() -> Self {
    let name = format!("ctl{}", std::env::consts::EXE_SUFFIX);
    let ctl = std::env::current_exe()
      .ok()
      .map(|executable| executable.with_file_name(&name))
      .filter(|program| program.is_file())
      .unwrap_or_else(|| name.into());
    Self { ctl }
  }
}

impl Workspace {
  #[cfg(test)]
  pub(super) fn with_ctl(ctl: impl Into<PathBuf>) -> Self {
    Self { ctl: ctl.into() }
  }

  pub(super) fn hosts(&self) -> io::Result<Vec<Host>> {
    let output = Command::new(&self.ctl)
      .args(["host", "list", "--json"])
      .output()
      .map_err(|error| {
        io::Error::new(
          error.kind(),
          format!("Could not run ctl host list: {error}"),
        )
      })?;
    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      let message = stderr.trim();
      return Err(io::Error::other(if message.is_empty() {
        format!("ctl host list exited with {}", output.status)
      } else {
        format!("ctl host list exited with {}: {message}", output.status)
      }));
    }
    parse_hosts(&output.stdout)
  }

  /// ctl owns saved-host resolution and streams the remote process over SSH.
  pub(super) fn ssh(&self, host: &Host, script: &str) -> Command {
    let mut command = Command::new(&self.ctl);
    command.args([
      "-H",
      &host.id,
      "--remote-platform",
      "unix",
      "exec",
      "sh",
      "-c",
      script,
    ]);
    command
  }

  pub(super) fn transport(&self, host: &Host, agent_path: &str) -> io::Result<SshCommand> {
    Ok(
      SshCommand::ctl_host(&host.id, self.ctl.clone())
        .with_agent(AgentLaunch::executable(agent_path)?.with_sibling_tools()),
    )
  }
}

fn parse_hosts(bytes: &[u8]) -> io::Result<Vec<Host>> {
  let views: Vec<HostView> = serde_json::from_slice(bytes).map_err(|error| {
    io::Error::new(
      io::ErrorKind::InvalidData,
      format!("Could not read ctl host list: {error}"),
    )
  })?;
  views
    .into_iter()
    .map(|HostView { host }| {
      if host.id.is_empty() || host.id.starts_with('-') || host.id.chars().any(char::is_control) {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "ctl host list returned an invalid saved host ID",
        ));
      }
      Ok(host)
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn saved_ids_are_used_for_hosts_with_the_same_label() {
    let hosts = parse_hosts(
      br#"[
        {"host":{"host_id":"first-id","name":"Work","connection_methods":[]},"statuses":[]},
        {"host":{"host_id":"second-id","name":"Work","remote_info":null},"statuses":[]}
      ]"#,
    )
    .unwrap();
    assert_eq!(hosts[0].name, hosts[1].name);

    let script = "printf '%s' 'a b'";
    let workspace = Workspace::with_ctl("test-ctl");
    let command = workspace.ssh(&hosts[1], script);
    assert_eq!(command.get_program(), "test-ctl");
    assert_eq!(
      command.get_args().collect::<Vec<_>>(),
      [
        "-H",
        "second-id",
        "--remote-platform",
        "unix",
        "exec",
        "sh",
        "-c",
        script
      ]
    );
  }

  #[test]
  fn malformed_host_ids_are_rejected_before_launch() {
    assert!(parse_hosts(br#"[{"host":{"host_id":"--help","name":"Work"}}]"#).is_err());
  }
}
