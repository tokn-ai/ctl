use super::*;
use std::time::Duration;

fn descriptor(output: &Path) -> serde_json::Value {
  serde_json::json!({
    "schema_version": 1,
    "target_triple": ctl_core::paths::native_target(),
    "binary_directory": output.join("bin"),
    "ctld_package": if cfg!(target_os = "macos") { Some(output.join("helper")) } else { None },
    "manager_executable": output.join("bin/ctlup"),
  })
}

#[test]
fn build_results_cannot_redirect_installation_inputs_outside_owned_staging() {
  let output = OutputDirectory::new().unwrap();
  let path = output.0.join("result.json");
  let valid = descriptor(&output.0);
  std::fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
  assert!(read_output(&output.0).is_ok());
  for field in ["binary_directory", "manager_executable", "ctld_package"] {
    let mut changed = valid.clone();
    changed[field] = serde_json::json!(output.0.join("../untrusted"));
    std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(read_output(&output.0).is_err(), "{field}");
  }
  std::fs::remove_file(&path).unwrap();
  std::os::unix::fs::symlink("missing", &path).unwrap();
  assert!(read_output(&output.0).is_err());
}

#[tokio::test]
async fn failed_builds_preserve_bounded_compiler_diagnostics() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let mut command = tokio::process::Command::new("sh");
  command.args([
    "-c",
    "printf 'discarded stdout'; printf 'fixture compiler failed\\n' >&2; exit 17",
  ]);
  let error = run_recipe(command, "native development build")
    .await
    .unwrap_err()
    .to_string();
  assert!(error.contains("17"));
  assert!(error.contains("fixture compiler failed"));
  assert!(error.contains("active installation was kept"));
  assert!(!error.contains("discarded stdout"));
}

#[tokio::test]
async fn cancelling_a_build_terminates_its_descendants() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let output = OutputDirectory::new().unwrap();
  let started = output.0.join("started");
  let escaped = output.0.join("escaped");
  let mut command = tokio::process::Command::new("sh");
  command
    .args([
      "-c",
      "(sleep 0.2; printf escaped > \"$ESCAPED\") & printf started > \"$STARTED\"; wait",
    ])
    .env("STARTED", &started)
    .env("ESCAPED", &escaped);
  let task = tokio::spawn(run_recipe(command, "native development build"));
  tokio::time::timeout(Duration::from_secs(5), async {
    while !started.exists() {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await
  .unwrap();
  task.abort();
  assert!(task.await.unwrap_err().is_cancelled());
  tokio::time::sleep(Duration::from_millis(350)).await;
  assert!(
    !escaped.exists(),
    "a cancelled native build left a child running"
  );
}
