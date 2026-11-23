use super::*;

#[test]
fn absolute_override_and_cargo_bin_work_without_a_local_runtime() {
  let home = tempfile::tempdir().unwrap();
  let cargo = home.path().join("cargo");
  let bin = cargo.join("bin");
  std::fs::create_dir_all(&bin).unwrap();
  let cargo_updater = bin.join(format!("ctlup{}", std::env::consts::EXE_SUFFIX));
  let explicit = home.path().join("custom");
  for path in [&cargo_updater, &explicit] {
    std::fs::write(path, []).unwrap();
  }
  for (override_path, expected) in [(Some(explicit.clone()), explicit), (None, cargo_updater)] {
    let (path, lease) = resolve_manager(home.path(), override_path, &cargo).unwrap();
    assert_eq!(path, expected.canonicalize().unwrap());
    assert!(lease.is_none());
  }
  assert!(resolve_manager(home.path(), Some(PathBuf::from("ctlup")), &cargo).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn an_incompatible_updater_never_receives_the_update_command() {
  use std::os::unix::fs::PermissionsExt as _;
  let root = tempfile::tempdir().unwrap();
  let path = root.path().join("ctlup");
  let marker = root.path().join("ctlup.ran");
  let (_stop, cancelled) = watch::channel(false);
  for layout in [MIN_TRACKED_UPDATE_LAYOUT - 1, MIN_TRACKED_UPDATE_LAYOUT] {
    let script = format!(
      "#!/bin/sh\ncase \"$1\" in\n--version) echo 'ctlup 0.1.4';;\ninstallation-layout-version) echo {layout};;\nupdate) touch \"$0.ran\";;\n*) exit 1;;\nesac\n"
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = run_compatible_manager(
      &path,
      "update",
      None,
      Duration::from_secs(5),
      cancelled.clone(),
    )
    .await;
    assert_eq!(result.is_ok(), layout == MIN_TRACKED_UPDATE_LAYOUT);
    assert_eq!(marker.exists(), layout == MIN_TRACKED_UPDATE_LAYOUT);
  }
}

#[cfg(unix)]
#[tokio::test]
async fn failed_updates_show_the_selected_hosts_error_and_remain_failures() {
  let (_stop, cancelled) = watch::channel(false);
  let row = r#"[{"host":"machine","error":"Selected account changed."}]"#;
  for (expected_host, output, message) in [
    (Some("machine"), row, "Selected account changed."),
    (Some("other"), row, "batch failed"),
    (None, row, "batch failed"),
    (Some("machine"), "invalid JSON", "batch failed"),
    (
      Some("machine"),
      r#"[{"host":"machine","error":"  "}]"#,
      "batch failed",
    ),
    (
      Some("machine"),
      r#"[{"host":"machine","error":"one"},{"host":"machine","error":"two"}]"#,
      "batch failed",
    ),
    (
      Some("machine"),
      r#"[{"host":"machine","check":{"state":"pinned"}}]"#,
      "batch failed",
    ),
  ] {
    let mut command = Command::new("/bin/sh");
    command.args([
      "-c",
      r#"printf '%s\n' "$1"; echo 'batch failed' >&2; exit 1"#,
      "fixture",
      output,
    ]);
    let error = run(
      command,
      Duration::from_secs(5),
      cancelled.clone(),
      expected_host,
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), message);
  }
}

#[cfg(unix)]
#[tokio::test]
async fn child_has_no_terminal_and_stop_interrupts_a_running_command() {
  let mut probe = Command::new("/bin/sh");
  probe.args(["-c", r#"test "$SSH_ASKPASS_REQUIRE" = never && ! read -r unused && ! (: </dev/tty) 2>/dev/null && printf ready"#]);
  let (stop, cancelled) = watch::channel(false);
  assert_eq!(
    run(probe, Duration::from_secs(5), cancelled.clone(), None)
      .await
      .unwrap(),
    b"ready"
  );
  let mut slow = Command::new("/bin/sh");
  slow.args(["-c", "sleep 60 & wait"]);
  let operation = tokio::spawn(run(slow, Duration::from_mins(1), cancelled, None));
  tokio::time::sleep(Duration::from_millis(50)).await;
  stop.send(true).unwrap();
  let error = tokio::time::timeout(Duration::from_secs(2), operation)
    .await
    .unwrap()
    .unwrap()
    .unwrap_err();
  assert!(error.to_string().contains("cancelled"));
}
