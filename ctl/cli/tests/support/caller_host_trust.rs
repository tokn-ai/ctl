use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use std::{
  ffi::OsString,
  fs,
  io::Write as _,
  path::{Path, PathBuf},
  process::{Command, Stdio},
  sync::mpsc::{self, Receiver},
  thread,
  time::{Duration, Instant},
};

pub const PASSWORD: &str = "  caller-credential-☃-canary  ";
pub const INPUT: &[u8] = b"command stdin stays separate\0\xff\n";
const LIMIT: Duration = Duration::from_secs(20);
const LAUNCH_SPEC: &str = "CTL_TEST_HOST_TRUST_LAUNCH";

pub struct Fixture {
  pub root: PathBuf,
  fake_ssh: bool,
}

pub struct Request<'a> {
  pub arguments: Vec<String>,
  pub environment: Vec<(OsString, OsString)>,
  pub password: Option<&'a str>,
  pub reply: Option<&'a str>,
  pub terminal: bool,
  pub input: &'a [u8],
}

pub struct Result {
  pub code: i32,
  pub stdout: Vec<u8>,
  pub transcript: String,
}

#[derive(Deserialize, Serialize)]
struct LaunchSpec {
  arguments: Vec<String>,
  stdin_path: PathBuf,
  stdout_path: PathBuf,
  status_path: PathBuf,
  no_terminal: bool,
}

impl Fixture {
  pub fn new(fake_ssh: bool) -> Self {
    #[cfg(unix)]
    let parent = PathBuf::from("/tmp");
    #[cfg(windows)]
    let parent = std::env::temp_dir();
    let root = parent.join(format!(
      "ctl-trust-{}",
      &uuid::Uuid::new_v4().simple().to_string()[..12]
    ));
    fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(root.join("key"), "synthetic supplied key").unwrap();
    fs::write(
      root.join("invalid-hosts.json"),
      "temporary destinations must not read this",
    )
    .unwrap();
    if fake_ssh {
      fs::write(
        root.join("ssh.rs"),
        include_str!("../fixtures/caller_ssh.rs"),
      )
      .unwrap();
      let output = Command::new("rustc")
        .args(["--edition=2024", "--crate-name", "caller_trust_ssh"])
        .arg(root.join("ssh.rs"))
        .arg("-o")
        .arg(root.join(format!("ssh{}", std::env::consts::EXE_SUFFIX)))
        .output()
        .unwrap();
      assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
      );
    }
    Self { root, fake_ssh }
  }

  pub fn run(&self, mut request: Request<'_>) -> Result {
    let directory = self.root.join(uuid::Uuid::new_v4().simple().to_string());
    fs::create_dir(&directory).unwrap();
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[cfg(unix)]
    let endpoint = directory.join("password.sock");
    #[cfg(windows)]
    let endpoint = PathBuf::from(format!(r"\\.\pipe\ctl-password-{}", uuid::Uuid::new_v4()));
    if request.password.is_some() {
      #[cfg(unix)]
      let option = "--password-socket";
      #[cfg(windows)]
      let option = "--password-pipe";
      request
        .arguments
        .splice(0..0, [option.into(), endpoint.to_str().unwrap().into()]);
    }
    let spec = LaunchSpec {
      arguments: request.arguments,
      stdin_path: directory.join("command-stdin"),
      stdout_path: directory.join("command-stdout"),
      status_path: directory.join("status"),
      no_terminal: !request.terminal,
    };
    fs::write(&spec.stdin_path, request.input).unwrap();
    let mut environment = request.environment;
    environment.extend([
      (
        LAUNCH_SPEC.into(),
        serde_json::to_string(&spec).unwrap().into(),
      ),
      (
        "CTL_HOSTS_PATH".into(),
        self.root.join("invalid-hosts.json").into_os_string(),
      ),
      (
        "CTLD_SOCKET_PATH".into(),
        self.root.join("must-not-start.sock").into_os_string(),
      ),
      (
        "CTLD_BIN".into(),
        self.root.join("must-not-start-ctld").into_os_string(),
      ),
    ]);
    if self.fake_ssh {
      let mut paths = vec![self.root.clone()];
      paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
      ));
      environment.push(("PATH".into(), std::env::join_paths(paths).unwrap()));
    }
    let mut process = Terminal::start(&environment, request.terminal, request.password);
    let handoff = request.password.map(|password| {
      let password = password.as_bytes().to_vec();
      thread::spawn(move || supply_password(&endpoint, &password))
    });
    if let Some(reply) = request.reply {
      process.wait_for("Are you sure you want to continue connecting");
      // Send a terminal Enter, including on ConPTY. The console confirmation
      // reader must return a line without requiring command stdin or another key.
      process.send(format!("{reply}\r").as_bytes());
    }
    let status = process.finish();
    if let Some(handoff) = handoff {
      handoff.join().unwrap();
    }
    assert!(
      status.success(),
      "launcher failed: {}",
      process.diagnostic()
    );
    let code = fs::read_to_string(&spec.status_path)
      .unwrap_or_else(|error| panic!("missing ctl status: {error}; {}", process.diagnostic()))
      .parse()
      .unwrap();
    let stdout = fs::read(&spec.stdout_path).unwrap();
    let transcript = String::from_utf8_lossy(&process.transcript).into_owned();
    if let Some(password) = request.password {
      assert!(
        !transcript.contains(password),
        "password disclosed on the terminal"
      );
      assert!(
        !stdout
          .windows(password.len())
          .any(|part| part == password.as_bytes()),
        "password disclosed on command stdout"
      );
    }
    Result {
      code,
      stdout,
      transcript,
    }
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.root);
  }
}

fn supply_password(endpoint: &Path, password: &[u8]) {
  tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap()
    .block_on(async {
      use tokio::io::AsyncWriteExt as _;
      let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
      loop {
        #[cfg(unix)]
        let connection = tokio::net::UnixStream::connect(endpoint).await;
        #[cfg(windows)]
        let connection = tokio::net::windows::named_pipe::ClientOptions::new().open(endpoint);
        match connection {
          Ok(mut stream) => {
            stream.write_all(password).await.unwrap();
            stream.shutdown().await.unwrap();
            break;
          }
          Err(error) => {
            assert!(
              tokio::time::Instant::now() < deadline,
              "password handoff failed: {error}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
          }
        }
      }
    });
}

/// A libtest child gives ctl a controlling terminal while its stdin is a pipe.
/// The serialized specification contains paths and arguments, never credentials.
pub fn child_launcher() {
  let Some(spec) = std::env::var_os(LAUNCH_SPEC) else {
    return;
  };
  let spec: LaunchSpec = serde_json::from_str(spec.to_str().unwrap()).unwrap();
  #[cfg(unix)]
  if spec.no_terminal {
    rustix::process::setsid().unwrap();
  }
  let mut command = Command::new(env!("CARGO_BIN_EXE_ctl"));
  command
    .args(&spec.arguments)
    .env_remove(LAUNCH_SPEC)
    .stdin(Stdio::piped())
    .stdout(fs::File::create(&spec.stdout_path).unwrap())
    .stderr(Stdio::inherit());
  #[cfg(windows)]
  if spec.no_terminal {
    use std::os::windows::process::CommandExt as _;
    command.creation_flags(0x0000_0008); // DETACHED_PROCESS: no inherited console.
  }
  let mut child = command.spawn().unwrap();
  child
    .stdin
    .take()
    .unwrap()
    .write_all(&fs::read(&spec.stdin_path).unwrap())
    .unwrap();
  let deadline = Instant::now() + LIMIT;
  let code = loop {
    if let Some(status) = child.try_wait().unwrap() {
      break status.code().unwrap_or(-1);
    }
    if Instant::now() >= deadline {
      terminate(&mut child);
      panic!("ctl did not finish within the fixture bound");
    }
    thread::sleep(Duration::from_millis(10));
  };
  fs::write(spec.status_path, code.to_string()).unwrap();
}

struct Terminal {
  child: Box<dyn Child + Send + Sync>,
  master: Option<Box<dyn MasterPty + Send>>,
  writer: Option<Box<dyn std::io::Write + Send>>,
  output: Receiver<Vec<u8>>,
  transcript: Vec<u8>,
  redact: Option<String>,
  #[cfg(windows)]
  cursor_answered: bool,
}

fn terminate(child: &mut dyn Child) {
  #[cfg(windows)]
  if let Some(pid) = child.process_id()
    && let Ok(mut killer) = Command::new("taskkill.exe")
      .args(["/PID", &pid.to_string(), "/T", "/F"])
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()
  {
    let deadline = Instant::now() + Duration::from_secs(5);
    while matches!(killer.try_wait(), Ok(None)) && Instant::now() < deadline {
      thread::sleep(Duration::from_millis(10));
    }
    if matches!(killer.try_wait(), Ok(None)) {
      let _ = killer.kill();
    }
  }
  let _ = child.kill();
  let deadline = Instant::now() + Duration::from_secs(5);
  while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
    thread::sleep(Duration::from_millis(10));
  }
}

fn read_output(mut reader: impl std::io::Read + Send + 'static, output: mpsc::Sender<Vec<u8>>) {
  thread::spawn(move || {
    let mut bytes = [0; 4096];
    while let Ok(count) = reader.read(&mut bytes) {
      if count == 0 || output.send(bytes[..count].to_vec()).is_err() {
        break;
      }
    }
  });
}

impl Terminal {
  fn start(environment: &[(OsString, OsString)], terminal: bool, password: Option<&str>) -> Self {
    let (sender, output) = mpsc::channel();
    let (child, master, writer) = if terminal {
      let pair = native_pty_system()
        .openpty(PtySize {
          rows: 40,
          cols: 160,
          ..PtySize::default()
        })
        .unwrap();
      let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
      command.args(["--exact", "caller_host_trust_child_launcher", "--nocapture"]);
      for (name, value) in environment {
        command.env(name, value);
      }
      let child = pair.slave.spawn_command(command).unwrap();
      drop(pair.slave);
      read_output(pair.master.try_clone_reader().unwrap(), sender);
      let writer = pair.master.take_writer().unwrap();
      (child, Some(pair.master), Some(writer))
    } else {
      let mut command = Command::new(std::env::current_exe().unwrap());
      command
        .args(["--exact", "caller_host_trust_child_launcher", "--nocapture"])
        .envs(environment.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
      let mut child = command.spawn().unwrap();
      read_output(child.stdout.take().unwrap(), sender.clone());
      read_output(child.stderr.take().unwrap(), sender);
      (Box::new(child) as Box<dyn Child + Send + Sync>, None, None)
    };
    Self {
      child,
      master,
      writer,
      output,
      transcript: Vec::new(),
      redact: password.map(str::to_owned),
      #[cfg(windows)]
      cursor_answered: false,
    }
  }

  fn diagnostic(&self) -> String {
    let text = String::from_utf8_lossy(&self.transcript).into_owned();
    self
      .redact
      .as_ref()
      .map_or_else(|| text.clone(), |secret| text.replace(secret, "[redacted]"))
  }

  fn record(&mut self, bytes: &[u8]) {
    assert!(
      self.transcript.len() + bytes.len() <= 64 * 1024,
      "terminal transcript exceeded its bound"
    );
    self.transcript.extend(bytes);
    #[cfg(windows)]
    if !self.cursor_answered
      && self.writer.is_some()
      && self.transcript.windows(4).any(|query| query == b"\x1b[6n")
    {
      // ConPTY may split its initial cursor-position query across reads and
      // wait for a response before the console child can display anything.
      self.send(b"\x1b[1;1R");
      self.cursor_answered = true;
    }
  }

  fn wait_for(&mut self, prompt: &str) {
    let deadline = Instant::now() + LIMIT;
    loop {
      if String::from_utf8_lossy(&self.transcript).contains(prompt) {
        return;
      }
      assert!(
        self.child.try_wait().unwrap().is_none(),
        "child exited before {prompt:?}: {}",
        self.diagnostic()
      );
      assert!(
        Instant::now() < deadline,
        "waiting for {prompt:?}: {}",
        self.diagnostic()
      );
      if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(10)) {
        self.record(&bytes);
      }
    }
  }

  fn send(&mut self, bytes: &[u8]) {
    let writer = self.writer.as_mut().unwrap();
    writer.write_all(bytes).unwrap();
    writer.flush().unwrap();
  }

  fn finish(&mut self) -> ExitStatus {
    let deadline = Instant::now() + LIMIT;
    let status = loop {
      if let Some(status) = self.child.try_wait().unwrap() {
        break status;
      }
      assert!(
        Instant::now() < deadline,
        "child did not exit: {}",
        self.diagnostic()
      );
      if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(10)) {
        self.record(&bytes);
      }
    };
    self.writer.take();
    self.master.take();
    loop {
      match self.output.recv_timeout(Duration::from_secs(2)) {
        Ok(bytes) => self.record(&bytes),
        Err(mpsc::RecvTimeoutError::Disconnected) => break,
        Err(mpsc::RecvTimeoutError::Timeout) => {
          panic!("terminal output stayed open: {}", self.diagnostic())
        }
      }
    }
    status
  }
}

impl Drop for Terminal {
  fn drop(&mut self) {
    if matches!(self.child.try_wait(), Ok(None)) {
      terminate(self.child.as_mut());
      self.writer.take();
      self.master.take();
    }
  }
}
