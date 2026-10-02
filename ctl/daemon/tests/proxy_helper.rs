#![cfg(unix)]

use ctl_ipc::{GatewayKind, SshGateway, SshGatewayMode};
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Stdio;

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let path = PathBuf::from("/tmp").join(format!("ctld-proxy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn gateway(kind: GatewayKind, destination: &str) -> SshGateway {
  SshGateway {
    kind,
    vpn: None,
    destination: destination.into(),
    hostname: None,
    user: None,
    port: Some(1080),
    identity_file: None,
    mode: SshGatewayMode::Automatic,
  }
}

fn encoded(route: &[SshGateway]) -> String {
  serde_json::to_vec(route)
    .unwrap()
    .iter()
    .fold(String::new(), |mut value, byte| {
      write!(value, "{byte:02x}").unwrap();
      value
    })
}

#[tokio::test]
async fn nested_ssh_proxy_uses_the_executing_helper_without_rediscovery() {
  let fixture = Fixture::new();
  // Spaces and quotes exercise the shell escaping in OpenSSH's ProxyCommand.
  let executable = fixture.0.join("selected ' ctld");
  std::fs::copy(env!("CARGO_BIN_EXE_ctld"), &executable).unwrap();
  let executable = executable.canonicalize().unwrap();
  let ssh = fixture.0.join("ssh");
  std::fs::write(
    &ssh,
    "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CTLD_TEST_PROXY_ARGS\"\nexit 0\n",
  )
  .unwrap();
  std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
  let prefix = [gateway(GatewayKind::Socks5, "socks.example.invalid")];
  let route = [
    prefix[0].clone(),
    gateway(GatewayKind::Ssh, "jump.example.invalid"),
  ];
  let marker = fixture.0.join("ssh-arguments");
  for explicit_override in [false, true] {
    let mut command = tokio::process::Command::new(&executable);
    command
      .args([
        "--proxy-route",
        &encoded(&route),
        "--proxy-host",
        "target.example.invalid",
        "--proxy-port",
        "2222",
      ])
      .env("PATH", &fixture.0)
      .env("HOME", &fixture.0)
      .env("CTLD_TEST_PROXY_ARGS", &marker)
      .env_remove("CTLD_BIN")
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS_SOCKET")
      .stdin(Stdio::null())
      .kill_on_drop(true);
    if explicit_override {
      command.env("CTLD_BIN", fixture.0.join("unrelated-ctld"));
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), command.output())
      .await
      .unwrap()
      .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert_eq!(output.stderr, Vec::<u8>::new());
    let arguments = std::fs::read_to_string(&marker).unwrap();
    let escaped = executable.to_string_lossy().replace('\'', "'\\''");
    let expected = format!(
      "ProxyCommand='{escaped}' --proxy-route {} --proxy-host %h --proxy-port %p",
      encoded(&prefix)
    );
    assert!(
      arguments.lines().any(|argument| argument == expected),
      "{arguments}"
    );
    assert!(!fixture.0.join(".tokn").exists());
  }
}
