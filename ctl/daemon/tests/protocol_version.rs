use std::process::Command;

#[test]
fn protocol_version_prints_the_ipc_version_and_exits() {
  let output = Command::new(env!("CARGO_BIN_EXE_ctld"))
    .arg("--protocol-version")
    .env_remove("CTLD_ASKPASS")
    .env_remove("CTLD_IDENTITY_ASKPASS")
    .output()
    .unwrap();
  assert!(output.status.success());
  assert_eq!(
    String::from_utf8(output.stdout).unwrap(),
    format!("{}\n", ctl_ipc::PROTOCOL_VERSION)
  );
  assert_eq!(output.stderr, Vec::<u8>::new());
}

#[test]
fn component_info_reports_embedded_build_and_all_apis_without_a_service() {
  let directory = std::env::temp_dir().join(format!("ctld-metadata-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir(&directory).unwrap();
  let output = Command::new(env!("CARGO_BIN_EXE_ctld"))
    .arg("--component-info")
    .arg("--socket")
    .arg(directory.join("ctld.sock"))
    .env_remove("CTLD_ASKPASS")
    .env_remove("CTLD_IDENTITY_ASKPASS")
    .output()
    .unwrap();
  assert!(output.status.success());
  let info: ctl_core::component::ComponentInfo = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(info.build, ctl_core::component::build_info());
  assert!(info.is_valid());
  assert_eq!(
    info.protocols,
    ctl_ipc::lifecycle::DaemonBinaryInfo::current().protocols
  );
  assert!(
    info
      .protocols
      .iter()
      .any(|protocol| protocol.name == "ctld" && protocol.version == ctl_ipc::PROTOCOL_VERSION)
  );
  assert!(
    info
      .protocols
      .iter()
      .any(|protocol| protocol.name == "ctld_lifecycle"
        && protocol.version == ctl_ipc::lifecycle::PROTOCOL_VERSION)
  );
  assert!(info.protocols.iter().any(
    |protocol| protocol.name == "ctld_helper" && protocol.version == ctl_ipc::HELPER_API_VERSION
  ));
  assert_eq!(output.stderr, Vec::<u8>::new());
  assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
  std::fs::remove_dir(directory).unwrap();
}

#[test]
fn protocol_build_prints_the_internal_build_and_exits() {
  let output = Command::new(env!("CARGO_BIN_EXE_ctld"))
    .arg("--protocol-build")
    .env_remove("CTLD_ASKPASS")
    .env_remove("CTLD_IDENTITY_ASKPASS")
    .output()
    .unwrap();
  assert!(output.status.success());
  assert_eq!(
    String::from_utf8(output.stdout).unwrap(),
    format!("{}\n", ctl_ipc::PROTOCOL_BUILD)
  );
  assert_eq!(output.stderr, Vec::<u8>::new());
}
