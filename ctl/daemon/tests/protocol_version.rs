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
  assert_eq!(output.stderr, Vec::<u8>::new());
}

#[test]
fn component_info_reports_embedded_build_and_both_protocols_without_a_service() {
  let output = Command::new(env!("CARGO_BIN_EXE_ctld"))
    .arg("--component-info")
    .env_remove("CTLD_ASKPASS")
    .output()
    .unwrap();
  assert!(output.status.success());
  let info: component_info::ComponentInfo = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(info.build, component_info::build_info());
  assert!(
    info
      .protocols
      .iter()
      .any(|protocol| protocol.name == "ctld" && protocol.version == ctld_ipc::PROTOCOL_VERSION)
  );
  assert!(
    info
      .protocols
      .iter()
      .any(|protocol| protocol.name == "ctld_lifecycle"
        && protocol.version == ctld_ipc::lifecycle::PROTOCOL_VERSION)
  );
  assert_eq!(output.stderr, Vec::<u8>::new());
}
