use std::process::Command;

#[test]
fn protocol_version_prints_the_ipc_version_and_exits() {
  let output = Command::new(env!("CARGO_BIN_EXE_ctld"))
    .arg("--protocol-version")
    .env_remove("CTLD_ASKPASS")
    .output()
    .unwrap();
  assert!(output.status.success());
  assert_eq!(
    String::from_utf8(output.stdout).unwrap(),
    format!("{}\n", ctld_ipc::PROTOCOL_VERSION)
  );
  assert!(output.stderr.is_empty());
}
