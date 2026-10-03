#![cfg(unix)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::termios::LocalModes;
use serde_json::Value;

struct Fixture {
  directory: PathBuf,
}

impl Fixture {
  fn new() -> Self {
    let directory = PathBuf::from("/tmp").join(format!("ctl-host-create-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    Self { directory }
  }

  fn catalog_path(&self) -> PathBuf {
    self.directory.join("hosts.json")
  }

  fn socket(&self) -> PathBuf {
    self.directory.join("ctld.sock")
  }

  fn stdout_path(&self) -> PathBuf {
    self.directory.join("stdout.json")
  }

  fn bytes(&self) -> Vec<u8> {
    fs::read(self.catalog_path()).unwrap()
  }

  fn hosts(&self) -> Vec<Value> {
    let snapshot: Value = serde_json::from_slice(&self.bytes()).unwrap();
    snapshot["document"]["hosts"].as_array().unwrap().clone()
  }

  fn create(&self, name: &str) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_ctl"))
      .args(["host", "create", name, "server", "--json"])
      .env("CTL_HOSTS_PATH", self.catalog_path())
      .env("CTLD_SOCKET_PATH", self.socket())
      .env("CTLD_BIN", self.directory.join("missing-ctld"))
      .stdin(Stdio::null())
      .output()
      .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
  }

  fn stdout(&self) -> Vec<u8> {
    fs::read(self.stdout_path()).unwrap()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.directory);
  }
}

struct Terminal {
  child: Box<dyn portable_pty::Child + Send + Sync>,
  master: Option<Box<dyn portable_pty::MasterPty + Send>>,
  writer: Option<Box<dyn std::io::Write + Send>>,
  output: std::sync::mpsc::Receiver<Vec<u8>>,
  transcript: Vec<u8>,
  next_prompt: usize,
}

impl Terminal {
  fn new(fixture: &Fixture, args: &[&str]) -> Self {
    let pair = portable_pty::native_pty_system()
      .openpty(portable_pty::PtySize {
        rows: 40,
        cols: 120,
        ..portable_pty::PtySize::default()
      })
      .unwrap();
    // Keep stdin and stderr attached to the PTY while collecting stdout
    // separately, so JSON assertions also cover redirected machine output.
    let mut command = portable_pty::CommandBuilder::new("/bin/sh");
    command.args([
      "-c",
      "exec \"$@\" > \"$CTL_HOST_TEST_STDOUT\"",
      "ctl-host-test",
    ]);
    command.arg(env!("CARGO_BIN_EXE_ctl"));
    command.args(args);
    command.env("CTL_HOST_TEST_STDOUT", fixture.stdout_path());
    command.cwd(&fixture.directory);
    command.env("TERM", "xterm-256color");
    command.env("CTLD_SOCKET_PATH", fixture.socket());
    command.env("CTLD_BIN", fixture.directory.join("missing-ctld"));
    command.env("CTL_HOSTS_PATH", fixture.catalog_path());
    let child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = pair.master.take_writer().unwrap();
    let (sender, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
      let mut buffer = [0; 4096];
      while let Ok(count) = reader.read(&mut buffer) {
        if count == 0 || sender.send(buffer[..count].to_vec()).is_err() {
          break;
        }
      }
    });
    Self {
      child,
      master: Some(pair.master),
      writer: Some(writer),
      output,
      transcript: Vec::new(),
      next_prompt: 0,
    }
  }

  fn wait_for(&mut self, prompt: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
      if String::from_utf8_lossy(&self.transcript[self.next_prompt..]).contains(prompt) {
        self.next_prompt = self.transcript.len();
        return;
      }
      let remaining = deadline.saturating_duration_since(Instant::now());
      match self.output.recv_timeout(remaining) {
        Ok(bytes) => self.transcript.extend(bytes),
        Err(error) => panic!(
          "did not receive prompt {prompt:?}: {error}\n{}",
          String::from_utf8_lossy(&self.transcript)
        ),
      }
    }
  }

  fn send(&mut self, keys: &str) {
    // Prompts are rendered before console enters raw mode. Wait until control
    // keys will reach the questionnaire instead of being interpreted as signals.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
      let termios = self.master.as_ref().unwrap().get_termios().unwrap();
      if termios.local_flags.bits() & (LocalModes::ICANON | LocalModes::ISIG).bits() == 0 {
        break;
      }
      assert!(
        Instant::now() < deadline,
        "terminal did not enter raw input mode before {keys:?}: {}",
        String::from_utf8_lossy(&self.transcript)
      );
      if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(1)) {
        self.transcript.extend(bytes);
      }
    }
    let writer = self.writer.as_mut().unwrap();
    writer.write_all(keys.as_bytes()).unwrap();
    writer.flush().unwrap();
  }

  fn finish(&mut self) -> portable_pty::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
      if let Some(status) = self.child.try_wait().unwrap() {
        break status;
      }
      assert!(
        Instant::now() < deadline,
        "terminal command did not exit: {}",
        String::from_utf8_lossy(&self.transcript)
      );
      if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(10)) {
        self.transcript.extend(bytes);
      }
    };
    self.writer.take();
    self.master.take();
    loop {
      match self.output.recv_timeout(Duration::from_secs(1)) {
        Ok(bytes) => self.transcript.extend(bytes),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
          panic!("terminal output did not close after process exit");
        }
      }
    }
    status
  }
}

impl Drop for Terminal {
  fn drop(&mut self) {
    if self.child.try_wait().ok().flatten().is_none() {
      let _ = self.child.kill();
      let _ = self.child.wait();
    }
  }
}

fn start_direct(terminal: &mut Terminal, name: &str) {
  terminal.wait_for("Host name");
  terminal.send(&format!("{name}\r"));
  terminal.wait_for("Connection type");
  terminal.send("\r");
  terminal.wait_for("SSH destination");
  terminal.send("server.example.test\r");
  terminal.wait_for("Username (optional)");
}

fn complete_defaults(terminal: &mut Terminal) {
  terminal.send("\r");
  terminal.wait_for("Port (optional)");
  terminal.send("\r");
  terminal.wait_for("Identity file (optional)");
  terminal.send("\r");
  terminal.wait_for("Connection method name");
  terminal.send("\r");
}

#[test]
fn direct_questionnaire_saves_custom_settings_and_keeps_json_output_clean() {
  let fixture = Fixture::new();
  let mut terminal = Terminal::new(&fixture, &["host", "create", "--json"]);
  start_direct(&mut terminal, "  Work host  ");
  terminal.send("  alice  \r");
  terminal.wait_for("Port (optional)");
  terminal.send("0\r");
  terminal.wait_for("Enter a port from 1 to 65535");
  terminal.send("\u{7f}2222\r");
  terminal.wait_for("Identity file (optional)");
  terminal.send("/keys/key with space\r");
  terminal.wait_for("Connection method name");
  terminal.send("Primary\r");
  terminal.wait_for("Host saved.");
  assert!(terminal.finish().success());

  let created: Value = serde_json::from_slice(&fixture.stdout()).unwrap();
  let method = &created["connection_methods"][0];
  assert_eq!(created["name"], "Work host");
  assert_eq!(created["preferred_method_id"], method["method_id"]);
  assert_eq!(method["name"], "Primary");
  assert!(method.get("ssh_config_alias").is_none());
  assert_eq!(method["target"]["destination"], "server.example.test");
  assert_eq!(method["target"]["user"], "alice");
  assert_eq!(method["target"]["port"], 2222);
  assert_eq!(method["target"]["identity_file"], "/keys/key with space");
  assert_eq!(fixture.hosts(), [created]);
  assert!(!fixture.socket().exists());
}

#[test]
fn alias_questionnaire_inherits_ssh_config_without_prompting_for_overrides() {
  let fixture = Fixture::new();
  let mut terminal = Terminal::new(&fixture, &["host", "create", "--json"]);
  terminal.wait_for("Host name");
  terminal.send("Work alias\r");
  terminal.wait_for("Connection type");
  terminal.send("\u{1b}[B\r");
  terminal.wait_for("SSH config alias");
  terminal.send("work-ssh-alias\r");
  terminal.wait_for("Connection method name");
  terminal.send("\r");
  terminal.wait_for("Host saved.");
  assert!(terminal.finish().success());

  let created: Value = serde_json::from_slice(&fixture.stdout()).unwrap();
  let method = &created["connection_methods"][0];
  assert_eq!(method["name"], "SSH");
  assert_eq!(method["ssh_config_alias"], "work-ssh-alias");
  assert_eq!(method["target"]["destination"], "work-ssh-alias");
  for field in ["hostname", "user", "port", "identity_file"] {
    assert!(method["target"].get(field).is_none(), "{field}");
  }
  let transcript = String::from_utf8_lossy(&terminal.transcript);
  for prompt in [
    "Username (optional)",
    "Port (optional)",
    "Identity file (optional)",
  ] {
    assert!(!transcript.contains(prompt), "{transcript}");
  }
  assert_eq!(fixture.hosts(), [created]);
  assert!(!fixture.socket().exists());
}

#[test]
fn cancelling_questionnaire_preserves_existing_hosts_and_creates_no_catalog_or_lock() {
  for existing_catalog in [false, true] {
    for cancel in ["\u{3}", "\u{1b}"] {
      let fixture = Fixture::new();
      let original = existing_catalog.then(|| {
        fixture.create("Existing host");
        fixture.bytes()
      });
      let mut terminal = Terminal::new(&fixture, &["host", "create", "--json"]);
      start_direct(&mut terminal, "Cancelled host");
      terminal.send(&format!("unfinished{cancel}"));
      terminal.wait_for("Cancelled. No changes made.");
      assert!(terminal.finish().success());
      assert_eq!(fixture.stdout(), Vec::<u8>::new());
      if let Some(original) = original {
        assert_eq!(fixture.bytes(), original);
      } else {
        assert!(!fixture.catalog_path().exists());
        assert!(!fixture.directory.join("workspace.lock").exists());
      }
      assert!(!fixture.socket().exists());
    }
  }
}

#[test]
fn questionnaire_preserves_unrelated_hosts_created_while_it_is_open() {
  let fixture = Fixture::new();
  let mut terminal = Terminal::new(&fixture, &["host", "create", "--json"]);
  start_direct(&mut terminal, "Questionnaire host");
  let other = fixture.create("Other host");
  complete_defaults(&mut terminal);
  terminal.wait_for("Host saved.");
  assert!(terminal.finish().success());

  let created: Value = serde_json::from_slice(&fixture.stdout()).unwrap();
  assert_eq!(fixture.hosts(), [other, created]);
}

#[test]
fn questionnaire_rejects_names_created_while_it_is_open_without_modifying_the_catalog() {
  let fixture = Fixture::new();
  let mut terminal = Terminal::new(&fixture, &["host", "create", "--json"]);
  start_direct(&mut terminal, "Duplicate host");
  fixture.create("Duplicate host");
  let original = fixture.bytes();
  complete_defaults(&mut terminal);
  assert!(!terminal.finish().success());
  assert_eq!(fixture.stdout(), Vec::<u8>::new());
  assert_eq!(fixture.bytes(), original);
  assert!(
    String::from_utf8_lossy(&terminal.transcript).contains("already in use"),
    "{}",
    String::from_utf8_lossy(&terminal.transcript)
  );
}

#[test]
fn partial_arguments_keep_explicit_settings_and_only_prompt_for_missing_fields() {
  let fixture = Fixture::new();
  let mut terminal = Terminal::new(
    &fixture,
    &[
      "host",
      "create",
      "Partial host",
      "--user",
      "alice",
      "--port",
      "2222",
      "--identity-file",
      "/keys/key with space",
      "--hostname",
      "10.0.0.20",
      "--vpn",
      "company",
      "--method-name",
      "SSH",
      "--json",
    ],
  );
  terminal.wait_for("Connection type");
  terminal.send("\r");
  terminal.wait_for("SSH destination");
  terminal.send("work\r");
  terminal.wait_for("Host saved.");
  assert!(terminal.finish().success());

  let created: Value = serde_json::from_slice(&fixture.stdout()).unwrap();
  let method = &created["connection_methods"][0];
  assert_eq!(created["name"], "Partial host");
  assert_eq!(method["name"], "SSH");
  assert_eq!(method["target"]["destination"], "work");
  assert_eq!(method["target"]["hostname"], "10.0.0.20");
  assert_eq!(method["target"]["user"], "alice");
  assert_eq!(method["target"]["port"], 2222);
  assert_eq!(method["target"]["identity_file"], "/keys/key with space");
  assert_eq!(method["target"]["vpn_connection_id"], "company");
  let transcript = String::from_utf8_lossy(&terminal.transcript);
  for prompt in [
    "Host name",
    "Username (optional)",
    "Port (optional)",
    "Identity file (optional)",
    "Connection method name",
  ] {
    assert!(!transcript.contains(prompt), "{transcript}");
  }
  assert_eq!(fixture.hosts(), [created]);
  assert!(!fixture.socket().exists());
}

#[test]
fn partial_arguments_keep_cleared_optional_fields_unset_without_prompting() {
  let fixture = Fixture::new();
  let mut terminal = Terminal::new(
    &fixture,
    &[
      "host",
      "create",
      "Default host",
      "--clear",
      "user,port,identity-file",
      "--method-name",
      "SSH",
      "--json",
    ],
  );
  terminal.wait_for("Connection type");
  terminal.send("\r");
  terminal.wait_for("SSH destination");
  terminal.send("work\r");
  terminal.wait_for("Host saved.");
  assert!(terminal.finish().success());

  let created: Value = serde_json::from_slice(&fixture.stdout()).unwrap();
  let method = &created["connection_methods"][0];
  assert_eq!(created["name"], "Default host");
  assert_eq!(method["name"], "SSH");
  assert_eq!(method["target"]["destination"], "work");
  for field in ["user", "port", "identity_file"] {
    assert!(method["target"].get(field).is_none(), "{field}");
  }
  let transcript = String::from_utf8_lossy(&terminal.transcript);
  for prompt in [
    "Username (optional)",
    "Port (optional)",
    "Identity file (optional)",
    "Connection method name",
  ] {
    assert!(!transcript.contains(prompt), "{transcript}");
  }
  assert_eq!(fixture.hosts(), [created]);
  assert!(!fixture.socket().exists());
}
