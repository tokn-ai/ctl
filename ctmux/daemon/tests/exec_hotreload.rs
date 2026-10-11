#![cfg(any(target_os = "macos", target_os = "linux"))]

use avt::Vt;
use ctmux_core::{alternate_screen::AlternateScreenTracker, mouse::TerminalInputModes};
use ctmux_process_info::Inspector;
use nix::{
  sys::signal::{Signal, killpg},
  unistd::{Pid, getpgrp},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
  fs,
  io::{BufRead, BufReader},
  os::unix::fs::DirBuilderExt,
  path::{Path, PathBuf},
  process::{Child, Command, Stdio},
  sync::mpsc::{self, Receiver},
  thread,
  time::{Duration, Instant},
};

// Six debug executable fingerprints and complete snapshots are CPU-heavy on
// virtualized Linux. Each actor I/O step still has its own five-second deadline.
const TIMEOUT: Duration = Duration::from_secs(60);

struct Proof {
  actor: Child,
  shell: Option<Inspector>,
  directory: PathBuf,
  reports: Receiver<Result<Value, String>>,
}

impl Proof {
  fn start(image_a: &Path, image_b: &Path, rounds: u32, fail_exec: bool) -> Self {
    let directory = std::env::temp_dir().join(format!("exec-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new()
      .mode(0o700)
      .create(&directory)
      .unwrap();
    let mut command = Command::new(image_a);
    command
      .arg(image_b)
      .arg(rounds.to_string())
      .env_clear()
      .env("PATH", "/usr/bin:/bin")
      .env("HOME", &directory)
      .env("TERM", "xterm")
      .current_dir(&directory)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::piped());
    if fail_exec {
      command.arg("--fail-exec");
    }
    let mut actor = command.spawn().unwrap();
    let stderr = actor.stderr.take().unwrap();
    let (sender, reports) = mpsc::channel();
    thread::spawn(move || {
      for line in BufReader::new(stderr).lines() {
        let report = line
          .map_err(|error| error.to_string())
          .and_then(|line| serde_json::from_str(&line).map_err(|error| format!("{error}: {line}")));
        if sender.send(report).is_err() {
          break;
        }
      }
    });
    Self {
      actor,
      shell: None,
      directory,
      reports,
    }
  }

  fn finish(mut self) -> Vec<Value> {
    let deadline = Instant::now() + TIMEOUT;
    let mut reports = Vec::new();
    loop {
      let remaining = deadline.saturating_duration_since(Instant::now());
      let report = self
        .reports
        .recv_timeout(remaining)
        .unwrap_or_else(|error| {
          panic!(
            "proof stopped reporting: {error}; progress: {:?}",
            recent_progress(&reports)
          )
        })
        .unwrap_or_else(|error| {
          panic!(
            "invalid proof report: {error}; progress: {:?}",
            recent_progress(&reports)
          )
        });
      assert_eq!(number(&report, "pid"), u64::from(self.actor.id()));
      if report["event"] == "started" {
        self.capture_shell(&report);
      }
      let completed = report["event"] == "completed";
      reports.push(report);
      assert!(reports.len() <= 512, "proof emitted too many reports");
      if completed {
        break;
      }
    }
    loop {
      if let Some(status) = self.actor.try_wait().unwrap() {
        assert!(
          status.success(),
          "proof process failed: {status}; progress: {:?}",
          recent_progress(&reports)
        );
        break;
      }
      assert!(
        Instant::now() < deadline,
        "proof did not exit after completion"
      );
      thread::sleep(Duration::from_millis(5));
    }
    if let Some(shell) = &self.shell {
      assert!(shell.inspect(None).is_err(), "proof left its shell running");
    }
    self.shell = None;
    reports
  }

  fn capture_shell(&mut self, report: &Value) {
    let pid = u32::try_from(number(report, "child_pid")).unwrap();
    // The proof may complete before the test consumes its first report.
    // Never capture a reused PID or terminate an unrelated process on failure.
    if let Ok(inspector) = Inspector::new(pid)
      && let Ok(snapshot) = inspector.inspect(None)
      && snapshot.shell.identity.start_time == number(report, "child_start_time")
      && snapshot.shell.parent_pid == self.actor.id()
    {
      self.shell = Some(inspector);
    }
  }

  fn terminate_shell(&self) {
    let Some(shell) = &self.shell else {
      return;
    };
    let Ok(snapshot) = shell.inspect(None) else {
      return;
    };
    let pid = snapshot.shell.identity.pid;
    if snapshot.shell.process_group != pid
      || snapshot.shell.process_group == u32::try_from(getpgrp().as_raw()).unwrap()
    {
      return;
    }
    let group = Pid::from_raw(i32::try_from(pid).unwrap());
    let _ = killpg(group, Signal::SIGTERM);
    for _ in 0..20 {
      if shell.inspect(None).is_err() {
        return;
      }
      thread::sleep(Duration::from_millis(5));
    }
    if shell.inspect(None).is_ok() {
      let _ = killpg(group, Signal::SIGKILL);
    }
  }
}

impl Drop for Proof {
  fn drop(&mut self) {
    self.terminate_shell();
    if self.actor.try_wait().ok().flatten().is_none() {
      let _ = self.actor.kill();
    }
    let _ = self.actor.wait();
    let _ = fs::remove_dir_all(&self.directory);
  }
}

fn number(report: &Value, field: &str) -> u64 {
  report[field]
    .as_u64()
    .unwrap_or_else(|| panic!("missing numeric {field}: {report}"))
}

fn recent_progress(reports: &[Value]) -> Vec<Value> {
  reports
    .iter()
    .rev()
    .take(8)
    .map(|report| {
      serde_json::json!({
        "event": report["event"],
        "image": report["image"],
        "generation": report["generation"],
        "error": report["error"],
      })
    })
    .collect()
}

fn example(name: &str) -> PathBuf {
  let path = Path::new(env!("CARGO_BIN_EXE_ctmuxd"))
    .parent()
    .unwrap()
    .join("examples")
    .join(name);
  assert!(
    path.is_file(),
    "build proof examples first: cargo build --locked -p ctmuxd --examples; missing {}",
    path.display()
  );
  path.canonicalize().unwrap()
}

fn fingerprint(path: &Path) -> String {
  format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn check_images(reports: &[Value], image_a: &Path, image_b: &Path, rounds: u32) {
  let images = [image_a, image_b];
  let hashes = [fingerprint(image_a), fingerprint(image_b)];
  assert_ne!(
    hashes[0], hashes[1],
    "proof must use distinct executable images"
  );
  let entries: Vec<_> = reports
    .iter()
    .filter(|report| matches!(report["event"].as_str(), Some("started" | "restored")))
    .collect();
  assert_eq!(entries.len(), usize::try_from(rounds + 1).unwrap());
  let original = entries[0];
  for (index, entry) in entries.iter().enumerate() {
    let image = index % 2;
    assert_eq!(entry["image"], if image == 0 { "A" } else { "B" });
    assert_eq!(entry["executable"], images[image].to_str().unwrap());
    assert_eq!(entry["executable_sha256"], hashes[image]);
    assert_eq!(number(entry, "generation"), u64::try_from(index).unwrap());
    assert_eq!(entry["fd_nonblocking"], true);
    assert_eq!(entry["fd_cloexec"], false);
    assert!(number(entry, "fd_count") <= number(original, "fd_count"));
    for field in [
      "pid",
      "child_pid",
      "child_start_time",
      "session_id",
      "view_id",
      "terminal_id",
      "fd_identity",
    ] {
      assert!(!original[field].is_null(), "missing {field}");
      assert_eq!(entry[field], original[field], "identity changed: {field}");
    }
  }
}

fn check_child_and_stream(reports: &[Value], rounds: u32, fail_exec: bool) {
  let identities: Vec<_> = reports
    .iter()
    .filter(|report| report["event"] == "shell_identity")
    .collect();
  assert_eq!(identities.len(), usize::try_from(rounds + 1).unwrap());
  for (index, identity) in identities.iter().enumerate() {
    assert_eq!(identity["shell_pid"], identity["child_pid"]);
    assert_eq!(identity["parent_pid"], identity["pid"]);
    assert_eq!(number(identity, "counter"), u64::try_from(index).unwrap());
    assert_eq!(identity["tty"], identities[0]["tty"]);
    assert_ne!(identity["tty"].as_str().unwrap(), "");
  }
  let completed = reports.last().unwrap();
  assert_eq!(completed["event"], "completed");
  assert_eq!(number(completed, "exit_code"), 37);
  assert_eq!(completed["waited_child_pid"], completed["child_pid"]);
  assert_eq!(completed["pending"], serde_json::json!([]));
  let history: Vec<_> = completed["history"]
    .as_array()
    .unwrap()
    .iter()
    .map(|line| line.as_str().unwrap())
    .collect();
  let records: Vec<_> = reports
    .iter()
    .filter(|report| report["event"] == "record")
    .map(|report| report["line"].as_str().unwrap())
    .collect();
  assert_eq!(
    history, records,
    "every consumed record must be committed exactly once"
  );
  let numbered: Vec<_> = history
    .iter()
    .copied()
    .filter(|line| line.starts_with("line:") || line.starts_with("partial:"))
    .collect();
  let expected: Vec<_> = (1..=rounds)
    .flat_map(|index| {
      let color = if index.is_multiple_of(2) {
        ""
      } else {
        "\x1b[38;2;12;34;56m"
      };
      [
        format!("line:{index}:complete"),
        format!("partial:{index}:{color}日:tail"),
      ]
    })
    .collect();
  assert_eq!(numbered, expected);
  assert_eq!(
    history.len(),
    usize::try_from(3 * rounds + 4 + u32::from(fail_exec)).unwrap()
  );
}

fn check_checkpoint_boundaries(reports: &[Value], rounds: u32) {
  let boundaries: Vec<_> = reports
    .iter()
    .filter(|report| report["event"] == "boundary")
    .collect();
  let restored: Vec<_> = reports
    .iter()
    .filter(|report| report["event"] == "restored")
    .collect();
  assert_eq!(boundaries.len(), usize::try_from(rounds).unwrap());
  for (index, (boundary, imported)) in boundaries.iter().zip(restored).enumerate() {
    let generation = index + 1;
    let mut pending = format!("partial:{generation}:").into_bytes();
    if generation.is_multiple_of(2) {
      pending.push(0xe6);
    } else {
      pending.extend_from_slice(b"\x1b[38;2;12;");
    }
    assert_eq!(boundary["pending"], serde_json::json!(pending));
    assert_eq!(imported["pending"], boundary["pending"]);
    assert_eq!(imported["history"], boundary["history"]);
    assert_eq!(imported["raw_output"], boundary["raw_output"]);
    assert_eq!(imported["terminal_state"], boundary["terminal_state"]);
  }
}

fn check_terminal_continuation(reports: &[Value], rounds: u32) {
  let mut previous_output = Vec::new();
  let mut snapshots = 0;
  for report in reports {
    let output: Vec<u8> = serde_json::from_value(report["raw_output"].clone()).unwrap();
    assert!(
      output.starts_with(&previous_output),
      "consumed stream changed"
    );
    previous_output.clone_from(&output);
    let snapshot = &report["terminal_state"];
    if snapshot.is_null() {
      continue;
    }
    snapshots += 1;
    assert_eq!(number(report, "checkpoint_version"), 2);

    // Replay the entire byte prefix independently of the actor's incremental
    // decoder and imports, including bytes that preceded earlier replacements.
    let (text, pending) = match std::str::from_utf8(&output) {
      Ok(text) => (text, &[][..]),
      Err(error) => {
        assert_eq!(error.error_len(), None, "invalid fixture UTF-8");
        let (valid, pending) = output.split_at(error.valid_up_to());
        (std::str::from_utf8(valid).unwrap(), pending)
      }
    };
    let mut vt = Vt::builder().size(12, 4).scrollback_limit(64).build();
    let mut input_modes = TerminalInputModes::default();
    for ch in text.chars() {
      vt.feed(ch);
      input_modes.feed(ch);
    }
    let mut alternate_screen = AlternateScreenTracker::default();
    alternate_screen.observe(&output);
    assert_eq!(
      snapshot["vt_state"],
      serde_json::to_value(vt.export_state()).unwrap()
    );
    assert_eq!(
      snapshot["input_modes_state"],
      serde_json::to_value(input_modes.export_state()).unwrap()
    );
    assert_eq!(
      snapshot["alternate_screen_state"],
      serde_json::to_value(alternate_screen.export_state()).unwrap()
    );
    assert_eq!(snapshot["utf8_pending"], serde_json::json!(pending));
    if report["event"] == "boundary" {
      let generation = number(report, "generation");
      assert_eq!(
        pending,
        if generation.is_multiple_of(2) {
          &[0xe6][..]
        } else {
          &[][..]
        }
      );
    }
  }
  assert_eq!(snapshots, usize::try_from(2 * rounds + 2).unwrap());
}

fn check_proof(rounds: u32, fail_exec: bool) -> Vec<Value> {
  let image_a = example("hotreload_a");
  let image_b = example("hotreload_b");
  let reports = Proof::start(&image_a, &image_b, rounds, fail_exec).finish();
  check_images(&reports, &image_a, &image_b, rounds);
  check_child_and_stream(&reports, rounds, fail_exec);
  check_checkpoint_boundaries(&reports, rounds);
  check_terminal_continuation(&reports, rounds);
  reports
}

#[test]
fn unix_exec_replaces_the_image_and_preserves_pty_child_and_buffered_io() {
  check_proof(2, false);
  check_proof(6, false);
}

#[test]
fn returned_exec_error_keeps_the_original_shell_usable_before_replacement() {
  let reports = check_proof(2, true);
  let failure = reports
    .iter()
    .position(|report| report["event"] == "exec_failed")
    .unwrap();
  let canary = reports
    .iter()
    .position(|report| report["event"] == "canary")
    .unwrap();
  let restored = reports
    .iter()
    .position(|report| report["event"] == "restored")
    .unwrap();
  assert!(failure < canary && canary < restored);
  assert_eq!(reports[failure]["image"], "A");
  assert_eq!(reports[canary]["image"], "A");
  assert_eq!(reports.last().unwrap()["failed_exec_verified"], true);
}
