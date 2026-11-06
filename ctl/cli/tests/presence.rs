use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Account(PathBuf);

impl Account {
  fn new() -> Self {
    let home = std::env::temp_dir().join(format!("ctl-inspection-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&home).unwrap();
    Self(home)
  }

  fn ctl(&self, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ctl"))
      .args(args)
      .env("HOME", &self.0)
      .env("USERPROFILE", &self.0)
      .env("CTLD_BIN", self.0.join("unavailable-broker"))
      .env("CTLD_SOCKET_PATH", self.0.join("no-daemon.sock"))
      .env("CTL_HOSTS_PATH", self.0.join("missing-host-catalog"))
      .output()
      .unwrap()
  }
}

impl Drop for Account {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn component_info_reports_the_consumed_contracts_without_initializing_the_account() {
  let account = Account::new();
  let output = account.ctl(&["--component-info"]);
  assert!(output.status.success(), "{output:?}");
  assert_eq!(output.stderr, Vec::<u8>::new());
  let info: ctl_core::component::ComponentInfo = serde_json::from_slice(&output.stdout).unwrap();
  assert!(info.is_valid());
  assert_eq!(info.build.version, env!("CARGO_PKG_VERSION"));
  let names: BTreeSet<_> = info
    .protocols
    .iter()
    .map(|protocol| protocol.name.as_str())
    .collect();
  assert_eq!(
    names,
    BTreeSet::from([
      "ctl_forwarding",
      "ctl_identity",
      "ctl_maintenance",
      "ctl_remote_vpn",
      "ctld",
      "ctld_lifecycle",
      "ctld_helper",
      "ctmux",
      "ctmux_control",
      "task",
      "task_control"
    ])
  );
  assert!(fs::read_dir(&account.0).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn who_reads_an_empty_account_and_rejects_target_options_before_any_connection_attempt() {
  let account = Account::new();
  let output = account.ctl(&["who", "--json"]);
  assert!(output.status.success(), "{output:?}");
  assert_eq!(
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
    serde_json::json!([])
  );
  for args in [
    &["--host", "unconfigured", "who"][..],
    &["who", "--host", "unconfigured"],
    &["--destination", "unconfigured", "who"],
    &["who", "--method", "other"],
    &["who", "--remote-platform", "unix"],
  ] {
    let output = account.ctl(args);
    assert!(!output.status.success());
    assert!(
      String::from_utf8(output.stderr)
        .unwrap()
        .contains("Run ctl who on the account")
    );
  }
  assert!(fs::read_dir(&account.0).unwrap().next().is_none());
}
