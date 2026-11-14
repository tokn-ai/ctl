use super::*;
use std::path::PathBuf;
use std::time::Duration;

struct Output(PathBuf);

impl Output {
  fn new() -> Self {
    let path =
      std::env::temp_dir().join(format!("ctlup-builder-job-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }
}

impl Drop for Output {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[tokio::test]
async fn failed_windows_builds_preserve_diagnostics() {
  let mut command = tokio::process::Command::new("node");
  command.args(["-e", "process.stdout.write('discarded stdout'); process.stderr.write('fixture compiler failed'); process.exitCode = 17"]);
  let error = run_recipe(command, "native development build")
    .await
    .unwrap_err()
    .to_string();
  assert!(error.contains("17"));
  assert!(error.contains("fixture compiler failed"));
  assert!(!error.contains("discarded stdout"));
}

#[tokio::test]
async fn cancelling_a_windows_build_terminates_its_descendants() {
  let output = Output::new();
  let started = output.0.join("started");
  let escaped = output.0.join("escaped");
  let mut command = tokio::process::Command::new("node");
  command
    .args([
      "-e",
      r#"
    require('node:child_process').spawn(process.execPath, ['-e',
      "setTimeout(() => require('node:fs').writeFileSync(process.env.ESCAPED, 'escaped'), 800)"
    ], { stdio: 'ignore' });
    require('node:fs').writeFileSync(process.env.STARTED, 'started');
    setInterval(() => {}, 1000);
  "#,
    ])
    .env("STARTED", &started)
    .env("ESCAPED", &escaped);
  let task = tokio::spawn(run_recipe(command, "native development build"));
  tokio::time::timeout(Duration::from_secs(10), async {
    while !started.exists() {
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .unwrap();
  task.abort();
  assert!(task.await.unwrap_err().is_cancelled());
  tokio::time::sleep(Duration::from_millis(1200)).await;
  assert!(
    !escaped.exists(),
    "a cancelled desktop build left a child running"
  );
}
