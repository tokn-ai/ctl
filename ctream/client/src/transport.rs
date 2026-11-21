#[cfg(target_os = "linux")]
use ctl_bundle::ctream::AGENT_DIRECTORY;
use ctl_bundle::ctream::AgentTarget;
use std::{
  ffi::OsString,
  io::{self, Read},
  path::{Path, PathBuf},
  process::{ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
  },
  thread,
  time::{Duration, Instant},
};

#[cfg(windows)]
use process_wrap::std::{ChildWrapper, CommandWrap, JobObject};

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
}

impl Process {
  fn close(self: &Arc<Self>) {
    if self.closing.swap(true, Ordering::AcqRel) {
      return;
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
          return;
        }
        if Instant::now() >= deadline {
          let _ = child.kill();
          let _ = child.wait();
          return;
        }
        drop(child);
        thread::sleep(Duration::from_millis(10));
      }
    });
  }

  fn wait(&self) -> io::Result<()> {
    loop {
      let status = self
        .child
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .try_wait()?;
      if let Some(status) = status {
        return if status.success() {
          Ok(())
        } else {
          Err(io::Error::other(format!(
            "agent channel exited with {status}"
          )))
        };
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
}

impl Read for ProcessReader {
  fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() {
      return Ok(0);
    }
    let count = self.stdout.read(buffer)?;
    if count == 0 {
      self.process.close();
      self.process.wait()?;
    }
    Ok(count)
  }
}

impl Drop for ProcessReader {
  fn drop(&mut self) {
    self.process.close();
  }
}

fn spawn(mut command: Command) -> io::Result<Channel<ProcessReader, ChildStdin>> {
  command.stdin(Stdio::piped()).stdout(Stdio::piped());
  let mut child = ChildProcess::spawn(command)?;
  let writer = child.take_stdin().expect("piped agent input");
  let stdout = child.take_stdout().expect("piped agent output");
  let process = Arc::new(Process {
    child: Mutex::new(child),
    closing: AtomicBool::new(false),
  });
  let cleanup = Arc::clone(&process);
  Ok(Channel {
    reader: ProcessReader { process, stdout },
    writer,
    cancellation: Cancellation::new(move || cleanup.close()),
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
    let mut command = Command::new(&self.agent);
    command.args(arguments);
    #[cfg(target_os = "linux")]
    if let Some(tools) = packaged_tools(&self.agent) {
      let inherited = std::env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
      let paths = std::iter::once(tools.to_path_buf()).chain(std::env::split_paths(&inherited));
      command.env(
        "PATH",
        std::env::join_paths(paths).map_err(io::Error::other)?,
      );
    }
    spawn(command)
  }
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
