//! Deliberately small, single-threaded proof of the exec/PTY boundary.
//!
//! Standard input and output become the same PTY master before exec. This lets
//! both images use safe descriptor APIs without constructing an owning handle
//! from a raw descriptor. Reports use standard error throughout. Production
//! daemons need a complete resource manifest and state codec instead.

use ctmux_process_info::Inspector;
use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::sys::stat::fstat;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{Pid, dup2, read, write};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::error::Error;
use std::io::{self, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{self, Command};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

mod terminal;
use terminal::{TerminalRuntime, TerminalState};

type ProofResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_INTERVAL: Duration = Duration::from_millis(2);
const EXIT_CODE: i32 = 37;
const CHECKPOINT_VERSION: u32 = 2;
const MAX_REPLACEMENTS: u32 = 6;
const MAX_CHECKPOINT_BYTES: usize = 120 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;
const SHELL_PROGRAM: &str = r#"
stty -echo || exit 90
sequence=0
printf 'primary-hist\033[6Sprimary-live\0337\033[?47halt-live\033[31m\0337\033[?47l\033[?1000h\033[?1005h\033[?1006h\033[?1015h\033[?1006l\033[?2004h\033[>4;2m\n'
printf 'ready:%s\n' "$$"
while IFS= read -r command; do
  case "$command" in
    step)
      sequence=$((sequence + 1))
      printf 'line:%s:complete\n' "$sequence"
      if [ $((sequence % 2)) -eq 1 ]; then
        printf 'partial:%s:\033[38;2;12;34;56m日:tail\n' "$sequence"
      else
        printf 'partial:%s:日:tail\n' "$sequence"
      fi
      ;;
    identity)
      printf 'identity:%s:%s:%s:%s\n' "$$" "$PPID" "$(tty)" "$sequence"
      ;;
    probe:*)
      printf 'probe:%s\n' "${command#probe:}"
      ;;
    quit:*)
      printf 'exit:%s\n' "${command#quit:}"
      exit 37
      ;;
    *) exit 91 ;;
  esac
done
exit 92
"#;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RuntimeState {
  checkpoint_version: u32,
  executable_a: PathBuf,
  executable_b: PathBuf,
  executable: PathBuf,
  executable_sha256: String,
  actor_pid: u32,
  child_pid: u32,
  child_start_time: u64,
  session_id: String,
  view_id: String,
  terminal_id: String,
  tty_name: String,
  fd_identity: [String; 3],
  generation: u32,
  replacements: u32,
  history: Vec<String>,
  pending: Vec<u8>,
  raw_output: Vec<u8>,
  terminal_state: TerminalState,
  fail_exec: bool,
  failed_exec_verified: bool,
  exit_nonce: String,
}

/// Run one image of the isolated proof; replacement discards this call stack.
///
/// # Errors
/// Returns an error for invalid arguments, failed restoration, failed PTY I/O,
/// or a child process whose identity, output, or exit status does not match.
pub fn run(image: &str) -> ProofResult {
  let arguments: Vec<String> = std::env::args().skip(1).collect();
  let (mut state, mut terminal) = if arguments.first().is_some_and(|value| value == "--restore") {
    if arguments.len() != 2 {
      return Err("restore requires exactly one checkpoint argument".into());
    }
    if arguments[1].len() > MAX_CHECKPOINT_BYTES {
      return Err("checkpoint exceeds the proof's encoded state limit".into());
    }
    let mut state: RuntimeState = serde_json::from_str(&arguments[1])?;
    validate_restore(image, &state)?;
    let terminal = TerminalRuntime::from_state(state.terminal_state.clone())?;
    state.terminal_state = terminal.export_state();
    update_executable_identity(&mut state)?;
    report(image, "restored", &state, json!({}))?;
    (state, terminal)
  } else {
    if image != "A" {
      return Err("image B must be started by image A".into());
    }
    initialize(&arguments)?
  };
  drive(image, &mut state, &mut terminal)
}

fn initialize(arguments: &[String]) -> ProofResult<(RuntimeState, TerminalRuntime)> {
  if !(2..=3).contains(&arguments.len())
    || arguments.get(2).is_some_and(|value| value != "--fail-exec")
  {
    return Err("usage: hotreload_a PATH_TO_HOTRELOAD_B REPLACEMENTS [--fail-exec]".into());
  }
  let replacements: u32 = arguments[1].parse()?;
  if !(1..=MAX_REPLACEMENTS).contains(&replacements) {
    return Err(format!("replacement count must be between 1 and {MAX_REPLACEMENTS}").into());
  }
  let pair = native_pty_system().openpty(PtySize::default())?;
  let mut command = CommandBuilder::new("/bin/sh");
  command.arg("-c");
  command.arg(SHELL_PROGRAM);
  let child = pair.slave.spawn_command(command)?;
  let child_pid = child.process_id().ok_or("PTY child has no process ID")?;
  let child_start_time = Inspector::new(child_pid)?
    .inspect(None)?
    .shell
    .identity
    .start_time;
  drop(pair.slave);
  let master = pair
    .master
    .as_raw_fd()
    .ok_or("PTY master has no descriptor")?;
  if master < 3 {
    return Err("the proof requires standard descriptors to be open before startup".into());
  }
  let tty_name = pair
    .master
    .tty_name()
    .ok_or("PTY master has no terminal name")?
    .to_string_lossy()
    .into_owned();
  dup2(master, 0)?;
  dup2(master, 1)?;
  let flags = OFlag::from_bits_retain(fcntl(0, FcntlArg::F_GETFL)?);
  fcntl(0, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
  // Never take portable-pty's writer: dropping it sends EOF to the shell.
  // These original handles are unnecessary once descriptors 0/1 own the master.
  drop(pair.master);
  drop(child);
  let mut terminal = TerminalRuntime::new();
  let mut state = RuntimeState {
    checkpoint_version: CHECKPOINT_VERSION,
    executable_a: std::env::current_exe()?,
    executable_b: PathBuf::from(&arguments[0]),
    executable: PathBuf::new(),
    executable_sha256: String::new(),
    actor_pid: process::id(),
    child_pid,
    child_start_time,
    session_id: Uuid::new_v4().to_string(),
    view_id: Uuid::new_v4().to_string(),
    terminal_id: Uuid::new_v4().to_string(),
    tty_name,
    fd_identity: descriptor_identity(0)?,
    generation: 0,
    replacements,
    history: Vec::new(),
    pending: Vec::new(),
    raw_output: Vec::new(),
    terminal_state: terminal.export_state(),
    fail_exec: arguments.len() == 3,
    failed_exec_verified: false,
    exit_nonce: Uuid::new_v4().to_string(),
  };
  update_executable_identity(&mut state)?;
  report("A", "started", &state, json!({}))?;
  let setup = receive_line("A", &mut state, &mut terminal)?;
  if !setup.starts_with("primary-hist") {
    return Err("the shell did not emit its terminal setup record".into());
  }
  let ready = receive_line("A", &mut state, &mut terminal)?;
  if ready != format!("ready:{child_pid}") {
    return Err(format!("unexpected shell greeting: {ready:?}").into());
  }
  Ok((state, terminal))
}

fn validate_restore(image: &str, state: &RuntimeState) -> ProofResult {
  if state.checkpoint_version != CHECKPOINT_VERSION
    || state.actor_pid != process::id()
    || state.generation == 0
    || state.generation > state.replacements
    || state.replacements > MAX_REPLACEMENTS
    || image
      != if state.generation.is_multiple_of(2) {
        "A"
      } else {
        "B"
      }
  {
    return Err("checkpoint image, version, generation, or PID mismatch".into());
  }
  verify_descriptors(state)?;
  if state.pending != boundary_prefix(state.generation) {
    return Err("checkpoint did not preserve the exact partially consumed record".into());
  }
  Ok(())
}

fn verify_descriptors(state: &RuntimeState) -> ProofResult {
  for descriptor in [0, 1] {
    if descriptor_identity(descriptor)? != state.fd_identity {
      return Err("inherited PTY identity changed".into());
    }
    if !OFlag::from_bits_retain(fcntl(descriptor, FcntlArg::F_GETFL)?).contains(OFlag::O_NONBLOCK)
      || FdFlag::from_bits_retain(fcntl(descriptor, FcntlArg::F_GETFD)?)
        .contains(FdFlag::FD_CLOEXEC)
    {
      return Err("inherited PTY descriptor flags changed".into());
    }
  }
  Ok(())
}

fn update_executable_identity(state: &mut RuntimeState) -> ProofResult {
  state.executable = std::env::current_exe()?;
  state.executable_sha256 = format!("{:x}", Sha256::digest(std::fs::read(&state.executable)?));
  Ok(())
}

fn descriptor_identity(descriptor: i32) -> ProofResult<[String; 3]> {
  let stat = fstat(descriptor)?;
  Ok([
    stat.st_dev.to_string(),
    stat.st_ino.to_string(),
    stat.st_rdev.to_string(),
  ])
}

fn drive(image: &str, state: &mut RuntimeState, terminal: &mut TerminalRuntime) -> ProofResult {
  if state.generation > 0 {
    let partial = receive_line(image, state, terminal)?;
    if partial != partial_record(state.generation) {
      return Err(format!("partial record was lost across exec: {partial:?}").into());
    }
  }
  observe_shell_identity(image, state, terminal)?;
  if state.fail_exec && !state.failed_exec_verified {
    probe_failed_exec(image, state, terminal)?;
  }
  if state.generation == state.replacements {
    return finish(image, state, terminal);
  }
  state.generation += 1;
  send_input(b"step\n")?;
  let complete = receive_line(image, state, terminal)?;
  if complete != format!("line:{}:complete", state.generation) {
    return Err(format!("unexpected shell output: {complete:?}").into());
  }
  receive_prefix(state, terminal, &boundary_prefix(state.generation))?;
  state.terminal_state = terminal.export_state();
  report(image, "boundary", state, json!({}))?;
  let target = if image == "A" {
    &state.executable_b
  } else {
    &state.executable_a
  };
  let checkpoint = serde_json::to_string(state)?;
  if checkpoint.len() > MAX_CHECKPOINT_BYTES {
    return Err("checkpoint exceeds the proof's bounded exec argument size".into());
  }
  let error = Command::new(target).arg("--restore").arg(checkpoint).exec();
  Err(format!("replacement exec failed: {error}").into())
}

fn probe_failed_exec(
  image: &str,
  state: &mut RuntimeState,
  terminal: &mut TerminalRuntime,
) -> ProofResult {
  let impossible_target = state.executable_b.join("nonexistent-hotreload-image");
  let error = Command::new(impossible_target).exec();
  if process::id() != state.actor_pid {
    return Err("failed exec changed the running PID".into());
  }
  verify_descriptors(state)?;
  report(
    image,
    "exec_failed",
    state,
    json!({ "error": error.to_string() }),
  )?;
  send_input(format!("probe:{}\n", state.exit_nonce).as_bytes())?;
  let canary = receive_line(image, state, terminal)?;
  if canary != format!("probe:{}", state.exit_nonce) {
    return Err("failed exec lost input/output to the original live shell".into());
  }
  state.failed_exec_verified = true;
  report(image, "canary", state, json!({ "line": canary }))
}

fn observe_shell_identity(
  image: &str,
  state: &mut RuntimeState,
  terminal: &mut TerminalRuntime,
) -> ProofResult {
  send_input(b"identity\n")?;
  let line = receive_line(image, state, terminal)?;
  let fields: Vec<&str> = line.split(':').collect();
  if fields.len() != 5
    || fields[0] != "identity"
    || fields[1].parse::<u32>()? != state.child_pid
    || fields[2].parse::<u32>()? != state.actor_pid
    || fields[3] != state.tty_name
    || fields[4].parse::<u32>()? != state.generation
  {
    return Err(format!("the shell's own process/TTY/state changed: {line:?}").into());
  }
  if Inspector::new(state.child_pid)?
    .inspect(None)?
    .shell
    .identity
    .start_time
    != state.child_start_time
  {
    return Err("the shell's original process birth identity changed".into());
  }
  report(
    image,
    "shell_identity",
    state,
    json!({
      "shell_pid": fields[1].parse::<u32>()?,
      "parent_pid": fields[2].parse::<u32>()?,
      "tty": fields[3],
      "counter": fields[4].parse::<u32>()?,
    }),
  )
}

fn finish(image: &str, state: &mut RuntimeState, terminal: &mut TerminalRuntime) -> ProofResult {
  send_input(format!("quit:{}\n", state.exit_nonce).as_bytes())?;
  let acknowledgement = receive_line(image, state, terminal)?;
  if acknowledgement != format!("exit:{}", state.exit_nonce) {
    return Err("the surviving shell did not acknowledge the final input nonce".into());
  }
  let child = Pid::from_raw(i32::try_from(state.child_pid)?);
  let deadline = Instant::now() + IO_TIMEOUT;
  loop {
    match waitpid(child, Some(WaitPidFlag::WNOHANG))? {
      WaitStatus::Exited(_, code) if code == EXIT_CODE => {
        state.terminal_state = terminal.export_state();
        return report(
          image,
          "completed",
          state,
          json!({ "exit_code": code, "waited_child_pid": state.child_pid }),
        );
      }
      WaitStatus::StillAlive if Instant::now() < deadline => thread::sleep(RETRY_INTERVAL),
      status => return Err(format!("unexpected shell exit status: {status:?}").into()),
    }
  }
}

fn receive_line(
  image: &str,
  state: &mut RuntimeState,
  terminal: &mut TerminalRuntime,
) -> ProofResult<String> {
  let deadline = Instant::now() + IO_TIMEOUT;
  loop {
    let byte = consume_byte(state, terminal, deadline)?;
    if byte == b'\n' {
      let mut bytes = std::mem::take(&mut state.pending);
      if bytes.last() == Some(&b'\r') {
        bytes.pop();
      }
      let line = String::from_utf8(bytes)?;
      state.history.push(line.clone());
      report(image, "record", state, json!({ "line": line }))?;
      return Ok(line);
    }
    state.pending.push(byte);
    if state.pending.len() > 1024 {
      return Err("PTY output exceeded the proof's record limit".into());
    }
  }
}

fn receive_prefix(
  state: &mut RuntimeState,
  terminal: &mut TerminalRuntime,
  expected: &[u8],
) -> ProofResult {
  if !state.pending.is_empty() {
    return Err("a completed record left unexpected buffered bytes".into());
  }
  let deadline = Instant::now() + IO_TIMEOUT;
  for expected_byte in expected {
    let byte = consume_byte(state, terminal, deadline)?;
    state.pending.push(byte);
    if byte != *expected_byte {
      return Err("shell did not emit the expected partial record prefix".into());
    }
  }
  Ok(())
}

fn partial_record(generation: u32) -> String {
  let color = if generation.is_multiple_of(2) {
    ""
  } else {
    "\x1b[38;2;12;34;56m"
  };
  format!("partial:{generation}:{color}日:tail")
}

fn boundary_prefix(generation: u32) -> Vec<u8> {
  let mut prefix = format!("partial:{generation}:").into_bytes();
  if generation.is_multiple_of(2) {
    prefix.push(0xe6);
  } else {
    prefix.extend_from_slice(b"\x1b[38;2;12;");
  }
  prefix
}

fn consume_byte(
  state: &mut RuntimeState,
  terminal: &mut TerminalRuntime,
  deadline: Instant,
) -> ProofResult<u8> {
  if state.raw_output.len() >= MAX_OUTPUT_BYTES {
    return Err("PTY output exceeds the proof's bounded stream size".into());
  }
  let byte = receive_byte(deadline)?;
  terminal.feed_byte(byte)?;
  state.raw_output.push(byte);
  Ok(byte)
}

fn receive_byte(deadline: Instant) -> ProofResult<u8> {
  let mut byte = [0];
  loop {
    if Instant::now() >= deadline {
      return Err("timed out waiting for PTY output".into());
    }
    match read(0, &mut byte) {
      Ok(1) => return Ok(byte[0]),
      Ok(_) => return Err("PTY closed before the expected output".into()),
      Err(Errno::EINTR) => {}
      Err(Errno::EAGAIN) if Instant::now() < deadline => thread::sleep(RETRY_INTERVAL),
      Err(error) => return Err(error.into()),
    }
  }
}

fn send_input(mut input: &[u8]) -> ProofResult {
  let deadline = Instant::now() + IO_TIMEOUT;
  while !input.is_empty() {
    if Instant::now() >= deadline {
      return Err("timed out sending PTY input".into());
    }
    match write(io::stdout(), input) {
      Ok(0) => return Err("PTY input write made no progress".into()),
      Ok(count) => input = &input[count..],
      Err(Errno::EINTR) => {}
      Err(Errno::EAGAIN) if Instant::now() < deadline => thread::sleep(RETRY_INTERVAL),
      Err(error) => return Err(error.into()),
    }
  }
  Ok(())
}

fn report(image: &str, event: &str, state: &RuntimeState, extra: serde_json::Value) -> ProofResult {
  let mut value = serde_json::to_value(state)?;
  let object = value.as_object_mut().ok_or("checkpoint is not an object")?;
  // The live runtime is exported only at explicit migration boundaries. Other
  // progress events must not accidentally present an older snapshot as current.
  if !matches!(event, "started" | "boundary" | "restored" | "completed") {
    object.remove("terminal_state");
  }
  object.insert("event".into(), event.into());
  object.insert("image".into(), image.into());
  object.insert("pid".into(), process::id().into());
  object.insert("fd_count".into(), open_descriptor_count()?.into());
  let mut nonblocking = true;
  let mut close_on_exec = false;
  for descriptor in [0, 1] {
    nonblocking &=
      OFlag::from_bits_retain(fcntl(descriptor, FcntlArg::F_GETFL)?).contains(OFlag::O_NONBLOCK);
    close_on_exec |=
      FdFlag::from_bits_retain(fcntl(descriptor, FcntlArg::F_GETFD)?).contains(FdFlag::FD_CLOEXEC);
  }
  object.insert("fd_nonblocking".into(), nonblocking.into());
  object.insert("fd_cloexec".into(), close_on_exec.into());
  if let serde_json::Value::Object(extra) = extra {
    object.extend(extra);
  }
  let mut output = io::BufWriter::new(io::stderr().lock());
  serde_json::to_writer(&mut output, &value)?;
  writeln!(output)?;
  output.flush()?;
  Ok(())
}

fn open_descriptor_count() -> ProofResult<u32> {
  // The directory iterator owns a descriptor while open. Close it before
  // checking which reported descriptors are still live so it is not counted.
  let entries = std::fs::read_dir("/dev/fd")?.collect::<io::Result<Vec<_>>>()?;
  let descriptors: Vec<i32> = entries
    .into_iter()
    .filter_map(|entry| {
      entry
        .file_name()
        .to_str()
        .and_then(|name| name.parse().ok())
    })
    .collect();
  Ok(u32::try_from(
    descriptors
      .into_iter()
      .filter(|descriptor| fcntl(*descriptor, FcntlArg::F_GETFD).is_ok())
      .count(),
  )?)
}
