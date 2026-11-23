use ctream_client::transport::{AgentLaunch, SshCommand};
use serde::Deserialize;
use std::{env, ffi::OsString, io, path::PathBuf, process::Command};

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
    Self {
      ctl: ctl_program(
        env::var_os("CTREAM_CTL_BIN"),
        env::current_exe().ok(),
        ctl_core::paths::directory().ok(),
      ),
    }
  }
}

fn ctl_program(
  configured: Option<OsString>,
  executable: Option<PathBuf>,
  profile: Option<PathBuf>,
) -> PathBuf {
  if let Some(program) = configured.filter(|program| !program.is_empty()) {
    return program.into();
  }
  let name = format!("ctl{}", env::consts::EXE_SUFFIX);
  let sibling = executable.map(|executable| executable.with_file_name(&name));
  let managed = profile.map(|profile| profile.join("bin").join(&name));
  [sibling, managed]
    .into_iter()
    .flatten()
    .find(|program| {
      program
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && ctl_core::private_fs::is_executable(&metadata))
    })
    .unwrap_or_else(|| name.into())
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
          format!("Could not run {} host list: {error}", self.ctl.display()),
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
  use std::{fs, time::SystemTime};

  struct Directory(PathBuf);

  impl Directory {
    fn new() -> Self {
      let time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
      let directory =
        Self(env::temp_dir().join(format!("ctream-ctl-{}-{time}", std::process::id())));
      fs::create_dir(&directory.0).unwrap();
      directory
    }
  }

  impl Drop for Directory {
    fn drop(&mut self) {
      fs::remove_dir_all(&self.0).unwrap();
    }
  }

  #[test]
  fn ctl_lookup_prioritizes_override_then_sibling_then_profile_then_path() {
    let root = Directory::new();
    let name = format!("ctl{}", env::consts::EXE_SUFFIX);
    let executable = root.0.join("build profile").join("ctream-preview");
    let profile = root.0.join("selected profile");
    let resolve =
      |configured| ctl_program(configured, Some(executable.clone()), Some(profile.clone()));
    assert_eq!(resolve(None), PathBuf::from(&name));

    let managed = profile.join("bin").join(&name);
    fs::create_dir_all(managed.parent().unwrap()).unwrap();
    fs::write(&managed, []).unwrap();
    #[cfg(unix)]
    assert_eq!(resolve(None), PathBuf::from(&name));
    ctl_core::private_fs::set_executable(&fs::File::open(&managed).unwrap()).unwrap();
    assert_eq!(resolve(None), managed);

    let sibling = executable.with_file_name(&name);
    fs::create_dir_all(sibling.parent().unwrap()).unwrap();
    fs::write(&sibling, []).unwrap();
    #[cfg(unix)]
    assert_eq!(resolve(None), managed);
    ctl_core::private_fs::set_executable(&fs::File::open(&sibling).unwrap()).unwrap();
    assert_eq!(resolve(None), sibling);
    assert_eq!(resolve(Some(OsString::new())), sibling);

    let configured = root.0.join("explicit ctl");
    assert_eq!(resolve(Some(configured.clone().into())), configured);
    let workspace = Workspace {
      ctl: resolve(Some(configured.clone().into())),
    };
    let error = workspace.hosts().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(error.to_string().contains(&*configured.to_string_lossy()));
  }

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
