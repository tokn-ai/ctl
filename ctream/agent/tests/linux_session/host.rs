use ctream_client::{
  Client,
  transport::{AgentLaunch, SshCommand},
};
use serde_json::Value;
use std::{
  fs, io,
  path::{Path, PathBuf},
  process::Command,
};
use uuid::Uuid;

const AGENT: &str = env!("CARGO_BIN_EXE_ctream-agent");

// The SSH-compatible launcher executes the same generated remote command with
// an isolated daemon and journal. Caller environments and state remain separate.
pub(super) const TEST_SSH: &str = r#"
set -eu
test "$#" -eq 3 && test "$1" = -T && test "$2" = fixture-display || exit 2
export XDG_STATE_HOME="$CTREAM_TEST_HOST_STATE"
export CTREAM_TEST_DAEMON_RUNTIME="$CTREAM_TEST_HOST_STATE/daemon"
exec sh -c "$3"
"#;

pub(super) struct HostFixture {
  root: PathBuf,
  state: PathBuf,
}

impl HostFixture {
  pub(super) fn new(name: &str) -> io::Result<Self> {
    if !cfg!(all(debug_assertions, feature = "test-fixtures")) {
      return Err(io::Error::other(
        "run desktop integration tests with docker/ctream/test.sh to isolate the test daemon",
      ));
    }
    let root = std::env::temp_dir().join(format!("ctream-{name}-{}", Uuid::new_v4().simple()));
    let state = root.join("host");
    fs::create_dir_all(&state)?;
    Ok(Self { root, state })
  }

  pub(super) fn state(&self) -> &Path {
    &self.state
  }

  pub(super) fn caller(&self, name: &str) -> io::Result<PathBuf> {
    let path = self.root.join(format!("caller-{name}"));
    fs::create_dir(&path)?;
    Ok(path)
  }

  pub(super) fn command(&self) -> Command {
    self.command_for(Path::new(AGENT))
  }

  pub(super) fn command_for(&self, executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command.env("XDG_STATE_HOME", &self.state);
    command.env("CTREAM_TEST_DAEMON_RUNTIME", self.state.join("daemon"));
    command
  }

  pub(super) fn client(&self) -> io::Result<Client<SshCommand>> {
    let transport = SshCommand::new("fixture-display")
      .with_program("sh".into())
      .with_arguments(vec![
        "-c".into(),
        format!("CTREAM_TEST_HOST_STATE=\"$0\"\n{TEST_SSH}").into(),
        self.state.as_os_str().into(),
      ])
      .with_agent(AgentLaunch::executable(AGENT)?);
    Ok(Client::new(transport))
  }
}

impl Drop for HostFixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.root);
  }
}

pub(super) fn activity_details(state: &Path) -> io::Result<Vec<Value>> {
  let output = Command::new(AGENT)
    .args(["activity", "--details", "--limit", "50"])
    .env("XDG_STATE_HOME", state)
    .output()?;
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(serde_json::from_slice(&output.stdout)?)
}
