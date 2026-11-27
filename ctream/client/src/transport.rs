#[cfg(target_os = "linux")]
use ctl_bundle::ctream::AGENT_DIRECTORY;
use ctl_bundle::ctream::AgentTarget;
use std::{
  collections::VecDeque,
  ffi::OsString,
  io::{self, Read, Write},
  path::{Path, PathBuf},
  process::{ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
  sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
  },
  thread,
  time::{Duration, Instant},
};

#[cfg(windows)]
use process_wrap::std::{ChildWrapper, CommandWrap, JobObject};

const STDERR_LIMIT: usize = 4 * 1024;
const STDERR_GRACE: Duration = Duration::from_millis(100);

/// The agent reported a terminal refusal, as opposed to a lost channel or
/// an unconfirmed outcome returned by a nested driver.
#[derive(Debug)]
struct AgentRefusal(String);

impl std::fmt::Display for AgentRefusal {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.0)
  }
}

impl std::error::Error for AgentRefusal {}

pub(crate) fn after_dispatch(error: io::Error) -> io::Error {
  if error.get_ref().is_some_and(|e| e.is::<AgentRefusal>()) {
    error
  } else {
    ctream_driver::UnconfirmedDelivery::error(error)
  }
}

#[cfg(all(test, windows))]
mod windows_tests;

/// A subprocess whose Windows SSH descendants are owned together with its launcher.
pub struct ChildProcess {
  #[cfg(windows)]
  child: Box<dyn ChildWrapper>,
  #[cfg(not(windows))]
  child: std::process::Child,
}

impl ChildProcess {
  pub fn spawn(command: Command) -> io::Result<Self> {
    #[cfg(windows)]
    let child = CommandWrap::from(command).wrap(JobObject).spawn()?;
    #[cfg(not(windows))]
    let child = {
      let mut command = command;
      command.spawn()?
    };
    Ok(Self { child })
  }

  pub fn take_stdin(&mut self) -> Option<ChildStdin> {
    #[cfg(windows)]
    return self.child.stdin().take();
    #[cfg(not(windows))]
    self.child.stdin.take()
  }

  pub fn take_stdout(&mut self) -> Option<ChildStdout> {
    #[cfg(windows)]
    return self.child.stdout().take();
    #[cfg(not(windows))]
    self.child.stdout.take()
  }

  pub fn take_stderr(&mut self) -> Option<ChildStderr> {
    #[cfg(windows)]
    return self.child.stderr().take();
    #[cfg(not(windows))]
    self.child.stderr.take()
  }

  pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
    #[cfg(windows)]
    return self.child.inner_mut().try_wait();
    #[cfg(not(windows))]
    self.child.try_wait()
  }

  pub fn wait(&mut self) -> io::Result<ExitStatus> {
    #[cfg(windows)]
    return self.child.inner_mut().wait();
    #[cfg(not(windows))]
    self.child.wait()
  }

  pub fn kill(&mut self) -> io::Result<()> {
    #[cfg(windows)]
    return self.child.start_kill();
    #[cfg(not(windows))]
    self.child.kill()
  }
}

#[cfg(windows)]
impl Drop for ChildProcess {
  fn drop(&mut self) {
    let _ = self.child.start_kill();
  }
}

pub struct Channel<R, W> {
  pub reader: R,
  pub writer: W,
  pub cancellation: Cancellation,
}

/// Requests cleanup after closing the channel input. Process transports allow a
/// bounded grace period for the agent to release its resources before killing it.
#[derive(Clone, Default)]
pub struct Cancellation(Option<Arc<CancellationState>>);

struct CancellationState {
  requested: AtomicBool,
  cancel: Box<dyn Fn() + Send + Sync>,
}

impl Cancellation {
  pub fn new(cancel: impl Fn() + Send + Sync + 'static) -> Self {
    Self(Some(Arc::new(CancellationState {
      requested: AtomicBool::new(false),
      cancel: Box::new(cancel),
    })))
  }

  pub fn cancel(&self) {
    if let Some(state) = &self.0
      && !state.requested.swap(true, Ordering::AcqRel)
    {
      (state.cancel)();
    }
  }
}

struct Process {
  child: Mutex<ChildProcess>,
  closing: AtomicBool,
  diagnostics: Arc<Diagnostics>,
}

#[derive(Default)]
struct Diagnostics {
  state: Mutex<DiagnosticState>,
  finished: Condvar,
  #[cfg(unix)]
  stop: Mutex<Option<io::PipeWriter>>,
}

#[derive(Default)]
struct DiagnosticState {
  tail: VecDeque<u8>,
  done: bool,
}

impl Diagnostics {
  fn drain(
    &self,
    mut pipe: ChildStderr,
    mut stderr: impl Write,
    #[cfg(unix)] stop: io::PipeReader,
  ) {
    let mut buffer = [0; STDERR_LIMIT];
    #[cfg(unix)]
    let mut deadline: Option<Instant> = None;
    loop {
      #[cfg(unix)]
      {
        use rustix::event::{PollFd, PollFlags, poll};
        use rustix::io::Errno;

        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
          break;
        }
        let mut fds = [
          PollFd::new(&pipe, PollFlags::IN),
          PollFd::new(&stop, PollFlags::IN),
        ];
        match poll(&mut fds, None) {
          Ok(_) => {}
          Err(Errno::INTR) => continue,
          Err(_) => break,
        }
        // Descendants can keep the stderr pipe open after the launcher exits.
        // Drain pending output without waiting for those descendants.
        if !fds[1].revents().is_empty() {
          deadline.get_or_insert_with(|| Instant::now() + STDERR_GRACE);
        }
        if fds[0].revents().is_empty() {
          break;
        }
      }
      let count = match pipe.read(&mut buffer) {
        Ok(0) => break,
        Ok(count) => count,
        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
        Err(_) => break,
      };
      {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let excess = (state.tail.len() + count).saturating_sub(STDERR_LIMIT);
        state.tail.drain(..excess);
        state.tail.extend(&buffer[..count]);
      }
      // A ctl SSH trust prompt may wait for /dev/tty without ending its line.
      let _ = stderr.write_all(&buffer[..count]);
      let _ = stderr.flush();
    }
    self
      .state
      .lock()
      .unwrap_or_else(|error| error.into_inner())
      .done = true;
    self.finished.notify_all();
  }

  fn launcher_exited(&self) {
    #[cfg(unix)]
    self
      .stop
      .lock()
      .unwrap_or_else(|error| error.into_inner())
      .take();
  }

  fn message(&self) -> Option<String> {
    let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
    // The launcher may exit while a descendant still owns the stderr pipe.
    let (mut state, _) = self
      .finished
      .wait_timeout_while(state, STDERR_GRACE, |state| !state.done)
      .unwrap_or_else(|error| error.into_inner());
    let message: String = String::from_utf8_lossy(state.tail.make_contiguous())
      .chars()
      .filter(|&character| character == '\n' || !character.is_control())
      .collect();
    let message = message.trim();
    (!message.is_empty()).then(|| message.to_owned())
  }
}

impl Process {
  fn close(self: &Arc<Self>) -> bool {
    if self.closing.swap(true, Ordering::AcqRel) {
      return false;
    }
    let process = Arc::clone(self);
    thread::spawn(move || {
      let deadline = Instant::now() + Duration::from_secs(1);
      loop {
        let mut child = process
          .child
          .lock()
          .unwrap_or_else(|error| error.into_inner());
        if matches!(child.try_wait(), Ok(Some(_))) {
          // A Windows launcher may leave descendants holding the stderr pipe.
          #[cfg(windows)]
          let _ = child.kill();
          break;
        }
        if Instant::now() >= deadline {
          let _ = child.kill();
          let _ = child.wait();
          break;
        }
        drop(child);
        thread::sleep(Duration::from_millis(10));
      }
      process.diagnostics.launcher_exited();
    });
    true
  }

  fn wait(&self, report_diagnostics: bool, delegated: bool) -> io::Result<()> {
    loop {
      let status = self
        .child
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .try_wait()?;
      if let Some(status) = status {
        self.diagnostics.launcher_exited();
        if status.success() {
          return Ok(());
        }
        let mut message = format!("agent channel exited with {status}");
        if report_diagnostics && let Some(diagnostic) = self.diagnostics.message() {
          message.push('\n');
          message.push_str(&diagnostic);
        }
        if delegated && status.code() == Some(i32::from(ctream_driver::DELEGATED_FAILURE_EXIT)) {
          return Err(io::Error::other(AgentRefusal(message)));
        }
        return Err(io::Error::other(message));
      }
      thread::sleep(Duration::from_millis(10));
    }
  }
}

/// Opens a duplex agent channel. Arguments are chosen by the protocol client.
pub trait Transport {
  type Reader: Read + Send + 'static;
  type Writer: io::Write + Send + 'static;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>>;
}

/// A pre-dispatch protocol rejection from a host that predates this operation.
pub fn unsupported_operation(error: &io::Error) -> bool {
  error.to_string().lines().last().is_some_and(|line| {
    matches!(
      line.strip_prefix("ctream-agent: ").unwrap_or(line),
      "usage: ctream-agent VERSION OPERATION [DISPLAY [WINDOW]]"
        | "usage: ctream-agent VERSION OPERATION"
    )
  })
}

pub struct Ssh(pub String);

/// The executable launched by a POSIX remote shell, independent of its platform.
/// An explicit WSL path can name a Windows executable without changing transport.
#[derive(Clone, Debug)]
pub struct AgentLaunch {
  executable: String,
  search_user_bin: bool,
  sibling_tools: bool,
}

impl Default for AgentLaunch {
  fn default() -> Self {
    Self {
      executable: "ctream-agent".into(),
      search_user_bin: true,
      sibling_tools: false,
    }
  }
}

impl AgentLaunch {
  pub fn executable(executable: impl Into<String>) -> io::Result<Self> {
    let executable = executable.into();
    if executable.is_empty() || executable.starts_with('-') || executable.contains('\0') {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid agent executable",
      ));
    }
    Ok(Self {
      executable,
      search_user_bin: false,
      sibling_tools: false,
    })
  }

  /// Prefer tools installed beside an absolute executable on the remote host.
  pub fn with_sibling_tools(mut self) -> Self {
    self.sibling_tools = true;
    self
  }

  fn command(&self, arguments: &[String]) -> String {
    let sibling = self
      .sibling_tools
      .then(|| self.executable.rsplit_once('/'))
      .flatten()
      .filter(|(dir, _)| dir.starts_with('/'));
    let mut command = if let Some((dir, _)) = sibling {
      format!(
        r#"exec env PATH={}:"$PATH:$HOME/.local/bin" "#,
        shell_argument(dir)
      )
    } else if self.search_user_bin {
      r#"exec env PATH="$PATH:$HOME/.local/bin" "#.to_owned()
    } else {
      "exec ".to_owned()
    };
    command.push_str(&shell_argument(&self.executable));
    for argument in arguments {
      command.push(' ');
      command.push_str(&shell_argument(argument));
    }
    command
  }
}

enum SshLauncher {
  OpenSsh,
  Compatible,
  CtlWorkspace,
}

/// SSH-compatible subprocess with an explicit executable and argument prefix.
pub struct SshCommand {
  destination: String,
  program: PathBuf,
  prefix_arguments: Vec<OsString>,
  launcher: SshLauncher,
  agent: AgentLaunch,
}

impl SshCommand {
  pub fn new(destination: impl Into<String>) -> Self {
    Self {
      destination: destination.into(),
      program: PathBuf::from("ssh"),
      prefix_arguments: Vec::new(),
      launcher: SshLauncher::OpenSsh,
      agent: AgentLaunch::default(),
    }
  }

  /// Custom programs receive `-T HOST COMMAND` after this prefix. Their own
  /// authentication and connection policy remains in control of the launcher.
  pub fn with_program(mut self, program: PathBuf) -> Self {
    self.program = program;
    self.launcher = SshLauncher::Compatible;
    self
  }

  /// Resolve the destination strictly as a saved host through ctl's workspace.
  pub fn ctl_host(host_id: impl Into<String>, program: PathBuf) -> Self {
    Self {
      program,
      launcher: SshLauncher::CtlWorkspace,
      ..Self::new(host_id)
    }
  }

  pub fn with_arguments(mut self, arguments: Vec<OsString>) -> Self {
    self.prefix_arguments = arguments;
    self
  }

  pub fn with_agent(mut self, agent: AgentLaunch) -> Self {
    self.agent = agent;
    self
  }

  fn command(&self, arguments: &[String]) -> io::Result<Command> {
    if self.destination.is_empty()
      || self.destination.starts_with('-')
      || self.destination.contains('\0')
      || self.program.as_os_str().is_empty()
    {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid SSH command or destination",
      ));
    }
    let mut command = Command::new(&self.program);
    command.args(&self.prefix_arguments);
    if matches!(self.launcher, SshLauncher::CtlWorkspace) {
      command.args([
        "-H",
        &self.destination,
        "--remote-platform",
        "unix",
        "exec",
        "sh",
        "-c",
      ]);
    } else {
      command.arg("-T");
      if matches!(self.launcher, SshLauncher::OpenSsh) {
        command.args(["-C", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--"]);
      }
      command.arg(&self.destination);
    }
    command.arg(self.agent.command(arguments));
    Ok(command)
  }
}

/// Agent subprocess connected through local standard-input/output pipes.
pub struct Local {
  pub agent: PathBuf,
}

impl Default for Local {
  fn default() -> Self {
    let name = format!("ctream-agent{}", std::env::consts::EXE_SUFFIX);
    let executable = std::env::current_exe().ok();
    let agent = local_agent(executable.as_deref(), &name, AgentTarget::native());
    Self { agent }
  }
}

fn local_agent(
  executable: Option<&Path>,
  name: &str,
  bundle_target: Option<AgentTarget>,
) -> PathBuf {
  if let Some(viewer_directory) = executable.and_then(Path::parent) {
    let sibling = viewer_directory.join(name);
    if sibling.is_file() {
      return sibling;
    }
    if let Some(target) = bundle_target {
      let bundled = target.directory(viewer_directory).join(target.agent());
      if bundled.is_file() {
        return bundled;
      }
    }
  }
  name.into()
}

#[cfg(target_os = "linux")]
fn packaged_tools(agent: &Path) -> Option<&Path> {
  let target = AgentTarget::native()?;
  let directory = agent.parent()?;
  (agent.is_absolute()
    && agent.file_name()? == target.agent()
    && directory.file_name()? == target.triple()
    && directory.parent()?.file_name()? == AGENT_DIRECTORY)
    .then_some(directory)
}

pub struct ProcessReader {
  process: Arc<Process>,
  stdout: ChildStdout,
  delegated: bool,
}

impl Read for ProcessReader {
  fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() {
      return Ok(0);
    }
    let count = self.stdout.read(buffer)?;
    if count == 0 {
      let report_diagnostics = self.process.close();
      self.process.wait(report_diagnostics, self.delegated)?;
    }
    Ok(count)
  }
}

impl Drop for ProcessReader {
  fn drop(&mut self) {
    self.process.close();
  }
}

fn spawn(command: Command) -> io::Result<Channel<ProcessReader, ChildStdin>> {
  spawn_with_stderr(command, io::stderr())
}

fn spawn_with_stderr(
  mut command: Command,
  stderr: impl Write + Send + 'static,
) -> io::Result<Channel<ProcessReader, ChildStdin>> {
  #[cfg(unix)]
  let (stop, signal) = io::pipe()?;
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  let mut child = ChildProcess::spawn(command)?;
  let writer = child.take_stdin().expect("piped agent input");
  let stdout = child.take_stdout().expect("piped agent output");
  let pipe = child.take_stderr().expect("piped agent diagnostics");
  let diagnostics = Arc::new(Diagnostics {
    #[cfg(unix)]
    stop: Mutex::new(Some(signal)),
    ..Diagnostics::default()
  });
  let drain = Arc::clone(&diagnostics);
  thread::spawn(move || {
    drain.drain(
      pipe,
      stderr,
      #[cfg(unix)]
      stop,
    );
  });
  let process = Arc::new(Process {
    child: Mutex::new(child),
    closing: AtomicBool::new(false),
    diagnostics,
  });
  let cleanup = Arc::clone(&process);
  Ok(Channel {
    reader: ProcessReader {
      process,
      stdout,
      delegated: false,
    },
    writer,
    cancellation: Cancellation::new(move || {
      cleanup.close();
    }),
  })
}

fn shell_argument(value: &str) -> String {
  format!("'{}'", value.replace('\'', "'\\''"))
}

impl Transport for Ssh {
  type Reader = ProcessReader;
  type Writer = ChildStdin;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    SshCommand::new(self.0.clone()).open(arguments)
  }
}

impl Transport for SshCommand {
  type Reader = ProcessReader;
  type Writer = ChildStdin;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    spawn(self.command(arguments)?)
  }
}

impl Transport for Local {
  type Reader = ProcessReader;
  type Writer = ChildStdin;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    self.open_child(arguments, None)
  }
}

impl Local {
  /// Open a host-owned child. A delegated child still obtains admission from
  /// the desktop daemon before touching the selected input backend.
  pub fn open_delegated(
    &self,
    arguments: &[String],
    key: &str,
  ) -> io::Result<Channel<ProcessReader, ChildStdin>> {
    self.open_child(arguments, Some(key))
  }

  fn open_child(
    &self,
    arguments: &[String],
    delegation: Option<&str>,
  ) -> io::Result<Channel<ProcessReader, ChildStdin>> {
    #[cfg(windows)]
    if delegation.is_none()
      && matches!(
        arguments.get(1).map(String::as_str),
        Some(
          "capture"
            | "capture-window"
            | "capture-with-driver"
            | "capture-stream"
            | "live-room"
            | "attributed-session"
            | "turn-room"
            | "turn-session"
            | "viewer-input"
            | "viewer-observe"
            | "input"
            | "input-window"
            | "input-sync"
            | "action"
        )
      )
    {
      // Launch the shared daemon outside the per-channel Windows job, so
      // closing its first participant cannot terminate everyone else's room.
      prepare_desktop_daemon(&self.agent)?;
    }
    let mut command = Command::new(&self.agent);
    command.args(arguments);
    command.env_remove("CTREAM_INPUT_DELEGATION");
    if let Some(key) = delegation {
      command.env("CTREAM_INPUT_DELEGATION", key);
    }
    #[cfg(target_os = "linux")]
    if let Some(tools) = packaged_tools(&self.agent) {
      let inherited = std::env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
      let paths = std::iter::once(tools.to_path_buf()).chain(std::env::split_paths(&inherited));
      command.env(
        "PATH",
        std::env::join_paths(paths).map_err(io::Error::other)?,
      );
    }
    let mut channel = spawn(command)?;
    channel.reader.delegated = delegation.is_some();
    Ok(channel)
  }
}

#[cfg(windows)]
fn prepare_desktop_daemon(agent: &Path) -> io::Result<()> {
  use std::os::windows::process::CommandExt;
  let mut child = Command::new(agent)
    .arg("ensure-daemon")
    .creation_flags(0x08000000) // CREATE_NO_WINDOW
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()?;
  let deadline = Instant::now() + Duration::from_secs(3);
  while child.try_wait()?.is_none() {
    if Instant::now() >= deadline {
      let _ = child.kill();
      let _ = child.wait();
      break;
    }
    thread::sleep(Duration::from_millis(10));
  }
  // Older agents can still handle attributed sessions.
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  #[cfg(unix)]
  use std::io::Write;

  #[test]
  fn local_agent_prefers_a_sibling_then_the_native_bundle_then_path() {
    let dir =
      std::env::temp_dir().join(format!("ctream-local-agent-fixture-{}", std::process::id()));
    let executable = dir.join("ctream-preview");
    let name = "ctream-agent.exe";
    let target = AgentTarget::WindowsX64;
    let bundled_dir = dir.join("ctream-agents/x86_64-pc-windows-msvc");
    std::fs::create_dir(&dir).unwrap();
    std::fs::create_dir_all(&bundled_dir).unwrap();

    assert_eq!(
      local_agent(Some(&executable), name, Some(target)),
      Path::new(name)
    );
    let bundled = bundled_dir.join(name);
    std::fs::write(&bundled, []).unwrap();
    assert_eq!(local_agent(Some(&executable), name, Some(target)), bundled);
    assert_eq!(local_agent(Some(&executable), name, None), Path::new(name));

    let sibling = dir.join(name);
    std::fs::write(&sibling, []).unwrap();
    assert_eq!(local_agent(Some(&executable), name, Some(target)), sibling);
    std::fs::remove_dir_all(dir).unwrap();
  }

  #[test]
  fn shell_arguments_preserve_metacharacters_as_literal_text() {
    assert_eq!(
      shell_argument("a'b $(command); value"),
      "'a'\\''b $(command); value'"
    );
  }

  #[test]
  fn ssh_program_prefix_and_agent_path_remain_separate_arguments() {
    let transport = SshCommand::new("test-host")
      .with_program(PathBuf::from("launcher"))
      .with_arguments(vec!["ssh".into()])
      .with_agent(AgentLaunch::executable("/mounted drive/agent's.exe").unwrap());
    let command = transport
      .command(&["1.0.5".into(), "capture".into(), "windows-1".into()])
      .unwrap();
    assert_eq!(command.get_program(), "launcher");
    assert_eq!(
      command.get_args().collect::<Vec<_>>(),
      [
        "ssh",
        "-T",
        "test-host",
        "exec '/mounted drive/agent'\\''s.exe' '1.0.5' 'capture' 'windows-1'",
      ]
    );
    let standard = SshCommand::new("test-host").command(&[]).unwrap();
    assert!(
      standard
        .get_args()
        .any(|argument| argument == "BatchMode=yes")
    );
    assert!(
      SshCommand::new("-oProxyCommand=anything")
        .command(&[])
        .is_err()
    );
    assert!(AgentLaunch::executable("-invalid").is_err());
  }

  #[cfg(unix)]
  #[test]
  fn remote_shell_preserves_literal_executable_and_arguments() {
    let launch = AgentLaunch::executable("/usr/bin/printf").unwrap();
    let command = launch.command(&["%s".into(), "a'b $(printf unexpected); literal".into()]);
    let output = Command::new("/bin/sh")
      .args(["-c", &command])
      .output()
      .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"a'b $(printf unexpected); literal");
  }

  #[test]
  fn workspace_agent_uses_saved_host_resolution_and_sibling_tools() {
    let command = SshCommand::ctl_host("saved-id", "ctl".into())
      .with_agent(
        AgentLaunch::executable("/simulated home/agent's bin/ctream-agent")
          .unwrap()
          .with_sibling_tools(),
      )
      .command(&["1.0.8".into(), "discover".into()])
      .unwrap();
    assert_eq!(command.get_program(), "ctl");
    let arguments = command.get_args().collect::<Vec<_>>();
    assert_eq!(
      arguments[..7],
      [
        "-H",
        "saved-id",
        "--remote-platform",
        "unix",
        "exec",
        "sh",
        "-c"
      ]
    );
    assert_eq!(
      arguments[7],
      r#"exec env PATH='/simulated home/agent'\''s bin':"$PATH:$HOME/.local/bin" '/simulated home/agent'\''s bin/ctream-agent' '1.0.8' 'discover'"#
    );
  }

  #[cfg(unix)]
  #[test]
  fn local_channel_is_duplex_and_reports_process_failure() {
    let local = Local {
      agent: PathBuf::from("/bin/sh"),
    };
    let Channel {
      mut reader,
      mut writer,
      ..
    } = local
      .open(&["-c".into(), "read value; printf '%s' \"$value\"".into()])
      .unwrap();
    writer.write_all(b"literal input\n").unwrap();
    drop(writer);
    let mut response = String::new();
    reader.read_to_string(&mut response).unwrap();
    assert_eq!(response, "literal input");
    let Channel {
      mut reader, writer, ..
    } = local.open(&["-c".into(), "exit 9".into()]).unwrap();
    drop(writer);
    assert!(reader.read_to_string(&mut String::new()).is_err());
  }

  #[cfg(unix)]
  #[test]
  fn subprocess_diagnostics_are_bounded_and_separate_from_stdout() {
    let mut command = Command::new("/bin/sh");
    command.args([
      "-c",
      r#"printf 'discarded prefix\n' >&2
        head -c 131072 /dev/zero >&2
        printf '\033Invalid capture backend option\n' >&2
        printf '\001\377'
        exit 9"#,
    ]);
    let Channel {
      mut reader, writer, ..
    } = spawn_with_stderr(command, io::sink()).unwrap();
    drop(writer);
    let mut frame = [0; 2];
    reader.read_exact(&mut frame).unwrap();
    assert_eq!(frame, [1, 255]);
    let error = reader.read(&mut frame).unwrap_err().to_string();
    assert!(error.contains("exit status: 9"));
    assert!(error.contains("Invalid capture backend option"));
    assert!(!error.contains("discarded prefix"));
    assert!(!error.contains('\u{1b}'));
    assert!(error.len() <= STDERR_LIMIT);
  }

  #[test]
  fn buffered_diagnostics_are_available_without_stderr_eof() {
    let diagnostics = Diagnostics::default();
    diagnostics
      .state
      .lock()
      .unwrap()
      .tail
      .extend(b"capture helper failed\n");
    let (send, receive) = std::sync::mpsc::channel();
    thread::spawn(move || {
      let _ = send.send(diagnostics.message());
    });
    assert_eq!(
      receive
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .as_deref(),
      Some("capture helper failed")
    );
  }

  #[cfg(unix)]
  #[test]
  fn stderr_reader_stops_when_a_descendant_keeps_the_pipe_open() {
    let mut command = Command::new("/bin/sh");
    command.args([
      "-c",
      "sleep 4 </dev/null >/dev/null & printf 'launcher failed\\n' >&2; exit 9",
    ]);
    let Channel { mut reader, .. } = spawn_with_stderr(command, io::sink()).unwrap();
    let error = reader.read(&mut [0]).unwrap_err().to_string();
    assert!(error.contains("launcher failed"));

    let diagnostics = &reader.process.diagnostics;
    let state = diagnostics.state.lock().unwrap();
    let (state, _) = diagnostics
      .finished
      .wait_timeout_while(state, Duration::from_secs(2), |state| !state.done)
      .unwrap();
    assert!(state.done);
  }

  #[cfg(unix)]
  #[test]
  fn output_eof_reaps_a_process_that_keeps_running() {
    let local = Local {
      agent: PathBuf::from("/bin/sh"),
    };
    let Channel {
      mut reader, writer, ..
    } = local
      .open(&["-c".into(), "exec 1>&-; exec sleep 30".into()])
      .unwrap();
    drop(writer);
    let started = Instant::now();
    assert!(reader.read_to_end(&mut Vec::new()).is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
      reader
        .process
        .child
        .lock()
        .unwrap()
        .try_wait()
        .unwrap()
        .is_some()
    );
  }

  #[test]
  fn cancellation_callback_runs_once_across_clones() {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&count);
    let cancellation = Cancellation::new(move || {
      observed.fetch_add(1, Ordering::SeqCst);
    });
    cancellation.clone().cancel();
    cancellation.cancel();
    assert_eq!(count.load(Ordering::SeqCst), 1);
  }
}
