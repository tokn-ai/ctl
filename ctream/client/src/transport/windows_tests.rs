use super::{Channel, spawn_with_stderr};
use std::{
  env,
  io::{self, BufRead, BufReader, Read, Write},
  process::{self, Command, Stdio},
  thread,
  time::Duration,
};

const ROLE: &str = "CTREAM_TEST_WINDOWS_TRANSPORT_ROLE";
const READY: &str = "stderr descendant ready";
const DIAGNOSTIC: &str = "launcher failed with descendant still running";

fn fixture(test: &str, role: &str) -> Command {
  let mut command = Command::new(env::current_exe().unwrap());
  command
    .args([
      "--exact",
      &format!("transport::windows_tests::{test}"),
      "--nocapture",
    ])
    .env(ROLE, role);
  command
}

#[test]
fn stderr_launcher_fixture() {
  if env::var(ROLE).as_deref() != Ok("launcher") {
    return;
  }
  let mut descendant = fixture("stderr_descendant_fixture", "descendant")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
    .unwrap();
  // Wait for the fixture to start through a pipe the launcher owns separately
  // from its protocol stdout. Only stderr is shared with the launcher.
  let ready = BufReader::new(descendant.stdout.take().unwrap())
    .lines()
    .any(|line| line.unwrap() == READY);
  assert!(ready, "the stderr descendant did not start");
  assert!(descendant.try_wait().unwrap().is_none());
  eprintln!("{DIAGNOSTIC}");
  process::exit(9);
}

#[test]
fn stderr_descendant_fixture() {
  if env::var(ROLE).as_deref() == Ok("descendant") {
    // Separate the marker from the test harness's partial progress line.
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "\n{READY}").unwrap();
    stdout.flush().unwrap();
    drop(stdout);
    thread::sleep(Duration::from_secs(30));
  }
}

#[test]
fn stderr_reader_stops_when_a_windows_job_descendant_keeps_the_pipe_open() {
  let command = fixture("stderr_launcher_fixture", "launcher");
  let Channel {
    mut reader, writer, ..
  } = spawn_with_stderr(command, io::sink()).unwrap();
  drop(writer);

  let error = reader.read_to_end(&mut Vec::new()).unwrap_err().to_string();
  assert!(error.contains(DIAGNOSTIC), "{error}");
  let process = &reader.process;
  let status = process.child.lock().unwrap().try_wait().unwrap().unwrap();
  assert_eq!(status.code(), Some(9));

  // Keep the reader's Process alive so its Drop cannot terminate the job.
  let diagnostics = &process.diagnostics;
  let state = diagnostics.state.lock().unwrap();
  let (state, _) = diagnostics
    .finished
    .wait_timeout_while(state, Duration::from_secs(3), |state| !state.done)
    .unwrap();
  assert!(state.done, "the descendant kept the stderr reader open");
}
