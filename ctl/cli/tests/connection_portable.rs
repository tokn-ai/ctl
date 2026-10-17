//! Native client behavior shared by Unix and Windows. Authenticated Windows
//! service routing is exercised separately by scripts/ci/windows-ssh.ps1.

use serde_json::json;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("ctl-portable-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let catalog = json!({
      "revision": "test", "document": {
        "schema_version": 1, "ssh_gateways": [], "hosts": [{
          "host_id": "saved-1", "name": "work", "preferred_method_id": "ssh",
          "connection_methods": [{
            "method_id": "ssh", "name": "Config", "ssh_config_alias": "configured",
            "target": { "kind": "ssh", "destination": "configured", "hostname": "192.0.2.20" }
          }]
        }]
      }
    });
    fs::write(
      root.join("hosts.json"),
      serde_json::to_vec(&catalog).unwrap(),
    )
    .unwrap();
    Self(root)
  }

  fn command(&self) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ctl"));
    command
      .env("CTL_HOSTS_PATH", self.0.join("hosts.json"))
      .env("CTLD_BIN", self.0.join("must-not-start-ctld"))
      .env("CTLD_SOCKET_PATH", self.0.join("ctld.sock"));
    command
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn openssh_config_alias_survives_a_saved_hostname_override() {
  let fixture = Fixture::new();
  let identity = fixture.0.join("key with spaces");
  fs::write(&identity, "fixture").unwrap();
  let identity = identity.to_string_lossy().replace('\\', "/");
  let config_path = fixture.0.join("ssh-config");
  fs::write(&config_path, format!(
    "Host configured\n  HostName configured.example.com\n  User configured-user\n  Port 2207\n  IdentityFile \"{identity}\"\n  ProxyJump configured-jump\n"
  )).unwrap();
  let catalog = fs::read(fixture.0.join("hosts.json")).unwrap();
  let output = fixture
    .command()
    .args(["ssh", "-G", "-F"])
    .arg(config_path)
    .arg("work")
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  let config = String::from_utf8(output.stdout).unwrap();
  for line in [
    "hostname 192.0.2.20".to_owned(),
    "user configured-user".into(),
    "port 2207".into(),
    "proxyjump configured-jump".into(),
    format!("identityfile {identity}"),
  ] {
    assert!(
      config.lines().any(|actual| actual == line),
      "missing {line}: {config}"
    );
  }
  assert_eq!(fs::read(fixture.0.join("hosts.json")).unwrap(), catalog);
  assert!(!fixture.0.join("ctld.sock").exists());
}

#[test]
fn openssh_explicit_endpoint_options_override_saved_defaults() {
  let fixture = Fixture::new();
  let output = fixture
    .command()
    .args(["ssh", "-G", "-F", "none", "-p", "2200", "-l", "bob", "work"])
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  let config = String::from_utf8(output.stdout).unwrap();
  for line in ["user bob", "hostname 192.0.2.20", "port 2200"] {
    assert!(
      config.lines().any(|actual| actual == line),
      "missing {line}: {config}"
    );
  }
}

#[test]
fn unknown_saved_hosts_fail_before_opening_ssh() {
  let fixture = Fixture::new();
  for destination in ["unknown.example.com", "192.0.2.1", "operator@work"] {
    let output = fixture
      .command()
      .env("PATH", fixture.0.join("no-executables"))
      .args(["-H", destination, "exec", "--", "unused"])
      .output()
      .unwrap();
    assert!(!output.status.success(), "{output:?}");
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("No saved ctl host"), "{diagnostic}");
    assert!(diagnostic.contains("--destination"), "{diagnostic}");
  }
  assert!(!fixture.0.join("ctld.sock").exists());
}

#[test]
fn local_exec_preserves_binary_input_output_and_exit_status() {
  let fixture = Fixture::new();
  let mut child = fixture
    .command()
    .args(["exec", "--"])
    .arg(std::env::current_exe().unwrap())
    .args(["--exact", "local_exec_fixture", "--ignored", "--nocapture"])
    .env("CTL_EXEC_FIXTURE", "1")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  let bytes = b"input with spaces\0\xff\r\n";
  child.stdin.take().unwrap().write_all(bytes).unwrap();
  let output = child.wait_with_output().unwrap();
  assert_eq!(output.status.code(), Some(37), "{output:?}");
  // The native test runner writes its header before the fixture starts.
  assert!(output.stdout.ends_with(bytes), "{output:?}");
  assert_eq!(output.stderr, b"fixture-stderr\0\xff");
}

#[test]
#[ignore = "private subprocess fixture"]
fn local_exec_fixture() {
  assert_eq!(std::env::var("CTL_EXEC_FIXTURE").as_deref(), Ok("1"));
  let mut bytes = Vec::new();
  std::io::stdin().read_to_end(&mut bytes).unwrap();
  std::io::stdout().write_all(&bytes).unwrap();
  std::io::stderr()
    .write_all(b"fixture-stderr\0\xff")
    .unwrap();
  std::process::exit(37);
}
