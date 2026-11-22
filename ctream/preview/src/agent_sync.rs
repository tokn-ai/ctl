use crate::workspace_hosts::{Host, Workspace};
use ctl_bundle::ctream::{AgentTarget as Target, MAX_AGENT_BYTES, active_managed_preview};
use ctream_client::transport::{ChildProcess, SshCommand};
use ctream_proto::{CAPTURE_CATALOG_VERSION, CaptureDriverDescriptor, CaptureSystem};
use sha2::{Digest, Sha256};
use std::{
  cmp::Reverse,
  fmt::{self, Write as _},
  fs::{self, File},
  io::{self, IsTerminal, Read, Write},
  path::{Path, PathBuf},
  process::{Command, Stdio},
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
  },
  thread,
  time::{Duration, Instant},
};

const CATALOG_LIMIT: u64 = 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(90);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(180);
const PROBE: &str = r#"
set -eu
if [ "$(uname -s)" != Linux ]; then
  echo 'Managed ctream requires a Linux or WSL SSH endpoint.' >&2
  exit 1
fi
case "$(uname -r | tr '[:upper:]' '[:lower:]')" in
  *microsoft*|*wsl*)
    if ! command -v wslpath >/dev/null; then
      echo 'Windows capture requires WSL interop and wslpath.' >&2
      exit 1
    fi
    powershell=$(command -v powershell.exe || :)
    if [ -z "$powershell" ]; then
      powershell=$(wslpath -a -u 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe') || powershell=
    fi
    if [ -z "$powershell" ] || [ ! -x "$powershell" ]; then
      echo 'Windows PowerShell is unavailable in the WSL login.' >&2
      exit 1
    fi
    if ! win_info=$("$powershell" -NoLogo -NoProfile -NonInteractive -Command '
      $ErrorActionPreference = "Stop"
      [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
      $arch = $env:PROCESSOR_ARCHITEW6432
      if (!$arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
      $user_home = [Environment]::GetFolderPath("UserProfile")
      if (!$user_home) { throw "Windows user profile directory is unavailable." }
      [Console]::Out.Write($arch + [char]10 + $user_home)
    ' </dev/null); then
      echo 'WSL could not start Windows PowerShell in the current login.' >&2
      exit 1
    fi
    newline='
'
    arch=${win_info%%"$newline"*}
    win_home=${win_info#*"$newline"}
    if [ "$arch" != AMD64 ]; then
      echo 'Managed ctream through WSL currently requires Windows x64.' >&2
      exit 1
    fi
    mapped_user_home=$(wslpath -a -u "$win_home")
    target="$ctream_windows_x64"
    store="${mapped_user_home%/}/.tokn/ctl/ctream/agents"
    ;;
  *)
    case "$(uname -m)" in
      x86_64) target="$ctream_linux_x64" ;;
      aarch64) target="$ctream_linux_arm64" ;;
      *)
        echo 'Managed ctream supports Linux x86_64 and aarch64.' >&2
        exit 1
        ;;
    esac
    case "${CTL_DATA_DIR:-}" in
      /*) ctl_data_dir=${CTL_DATA_DIR%/} ;;
      *) ctl_data_dir="$HOME/.tokn/ctl" ;;
    esac
    store="$ctl_data_dir/ctream/agents"
    ;;
esac
printf 'ctream-sync-v1\n%s\n%s\n' "$target" "$store"
"#;

fn probe_script() -> String {
  format!(
    "ctream_windows_x64={}\nctream_linux_x64={}\nctream_linux_arm64={}\n{PROBE}",
    shell(Target::WindowsX64.triple()),
    shell(Target::LinuxX64.triple()),
    shell(Target::LinuxArm64.triple()),
  )
}

fn capture_systems(target: Target) -> &'static [CaptureSystem] {
  if target.is_windows() {
    &[CaptureSystem::Windows]
  } else {
    &[CaptureSystem::X11, CaptureSystem::Wayland]
  }
}

struct Endpoint {
  target: Target,
  store: String,
}

impl Endpoint {
  fn parse(data: &[u8]) -> io::Result<Self> {
    let mut lines = std::str::from_utf8(data).map_err(io::Error::other)?.lines();
    let marker = lines.next();
    let triple = lines.next();
    let store = lines.next();
    let target = triple
      .and_then(Target::from_triple)
      .ok_or_else(|| io::Error::other("Unexpected ctream host platform response"))?;
    if marker != Some("ctream-sync-v1") || lines.next().is_some() {
      return Err(io::Error::other("Unexpected ctream host platform response"));
    }
    let store = store
      .filter(|store| {
        store.starts_with('/')
          && !store.ends_with('/')
          && store.len() < 4096
          && !store.chars().any(char::is_control)
      })
      .ok_or_else(|| io::Error::other("The host returned an invalid agent directory"))?;
    Ok(Self {
      target,
      store: store.into(),
    })
  }
}

struct Artifact {
  name: &'static str,
  bytes: Arc<[u8]>,
  hash: String,
}

impl Artifact {
  fn new(name: &'static str, bytes: impl Into<Arc<[u8]>>) -> Self {
    let bytes = bytes.into();
    let hash = format!("{:x}", Sha256::digest(&bytes));
    Self { name, bytes, hash }
  }

  fn read(path: &Path, name: &'static str) -> io::Result<Option<Self>> {
    let path = path.join(name);
    let metadata = match fs::metadata(&path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
      Err(error) => return Err(error),
    };
    if !metadata.is_file() || !(1..=MAX_AGENT_BYTES).contains(&metadata.len()) {
      return Err(io::Error::other(format!(
        "Invalid ctream artifact {}",
        path.display()
      )));
    }
    let mut bytes = Vec::new();
    File::open(&path)?
      .take(MAX_AGENT_BYTES + 1)
      .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_AGENT_BYTES {
      return Err(io::Error::other(format!(
        "Invalid ctream artifact {}",
        path.display()
      )));
    }
    Ok(Some(Self::new(name, bytes)))
  }
}

fn artifact_directories(executable: &Path, target: Target) -> Vec<PathBuf> {
  let Some(parent) = executable.parent() else {
    return Vec::new();
  };
  let mut roots = vec![target.directory(parent)];
  // Cargo puts the viewer in <target-dir>/<profile> or in
  // <target-dir>/<viewer-triple>/<profile> for an explicit viewer target.
  if parent
    .file_name()
    .is_some_and(|name| name == "debug" || name == "release")
    && let Some(target_dir) = parent.parent()
  {
    let target_dir = if target_dir
      .file_name()
      .and_then(|name| name.to_str())
      .is_some_and(|name| {
        name.contains("-apple-")
          || name.contains("-unknown-linux-")
          || name.contains("-pc-windows-")
      }) {
      target_dir.parent().unwrap_or(target_dir)
    } else {
      target_dir
    };
    let mut triples = vec![target.triple()];
    if target.is_windows() {
      triples.push("x86_64-pc-windows-gnu");
    }
    for triple in triples {
      for profile in ["release", "debug"] {
        roots.push(target_dir.join(triple).join(profile));
      }
    }
    // Native Cargo builds have no triple directory. Match OS and architecture
    // before using this fallback, otherwise a macOS agent stub could be chosen.
    if Target::native() == Some(target) {
      roots.push(parent.to_path_buf());
      roots.push(target_dir.join("release"));
      roots.push(target_dir.join("debug"));
    }
  }
  roots
}

fn artifacts(executable: &Path, target: Target) -> io::Result<(PathBuf, Vec<Artifact>)> {
  let mut roots = artifact_directories(executable, target);
  // Keep bundled agents first. In a Cargo checkout a new debug build can be
  // newer than a leftover release build, or vice versa.
  if let Some((_, development)) = roots.split_first_mut() {
    development.sort_by_cached_key(|root| {
      Reverse(
        fs::metadata(root.join(target.agent()))
          .and_then(|metadata| metadata.modified())
          .ok(),
      )
    });
  }
  for root in roots {
    if let Some(artifacts) = artifacts_in(&root, target)? {
      return Ok(artifacts);
    }
  }
  // A development install can activate a new build while setup still runs
  // from the previous generation. Look up the current selection on each try.
  if let Some(active) = active_managed_preview(executable)?
    && active != executable
    && let Some(parent) = active.parent()
    && let Some(artifacts) = artifacts_in(&target.directory(parent), target)?
  {
    return Ok(artifacts);
  }
  Err(missing_agent_error(target))
}

fn artifacts_in(root: &Path, target: Target) -> io::Result<Option<(PathBuf, Vec<Artifact>)>> {
  let Some(agent) = Artifact::read(root, target.agent())? else {
    return Ok(None);
  };
  let mut files = vec![agent];
  for name in target.companions() {
    if let Some(file) = Artifact::read(root, name)? {
      files.push(file);
    }
  }
  Ok(Some((root.join(target.agent()), files)))
}

#[derive(Debug)]
struct MissingAgent(Target);

impl fmt::Display for MissingAgent {
  fn fmt(&self, message: &mut fmt::Formatter<'_>) -> fmt::Result {
    let target = self.0;
    let dev_target = if target.is_windows() && !cfg!(windows) {
      "x86_64-pc-windows-gnu"
    } else {
      target.triple()
    };
    write!(
      message,
      "No agent build for {}. For a development install, run ctlup dev --ctream --agent-target {dev_target}, then click Refresh. The bundle should contain {} beside ctream-preview.",
      target.triple(),
      target.relative_path(target.agent()),
    )
  }
}

impl std::error::Error for MissingAgent {}

pub(super) fn missing_agent_error(target: Target) -> io::Error {
  io::Error::new(io::ErrorKind::NotFound, MissingAgent(target))
}

pub(super) fn is_missing_agent(error: &io::Error) -> bool {
  error
    .get_ref()
    .is_some_and(|error| error.is::<MissingAgent>())
}

fn catalog_error(agent: &Path, error: io::Error) -> io::Error {
  let detail = error.to_string();
  if detail.contains("usage: ctream-agent VERSION OPERATION") {
    return io::Error::new(
      io::ErrorKind::Unsupported,
      format!(
        "The agent build at {} rejected the ctream_agent {CAPTURE_CATALOG_VERSION} driver catalog request. Rebuild ctream-agent from this checkout or use a preview bundle with its included agents.",
        agent.display()
      ),
    );
  }
  io::Error::new(
    error.kind(),
    format!(
      "Could not start the synchronized agent from {}: {detail}",
      agent.display()
    ),
  )
}

fn release_key(files: &[Artifact]) -> String {
  let mut hash = Sha256::new();
  for artifact in files {
    hash.update(artifact.name);
    hash.update([0]);
    hash.update(&artifact.hash);
    hash.update([0]);
  }
  format!("{:x}", hash.finalize())
}

fn shell(value: &str) -> String {
  format!("'{}'", value.replace('\'', "'\\''"))
}

struct Install<'a> {
  endpoint: &'a Endpoint,
  dir: String,
  files: &'a [Artifact],
}

impl Install<'_> {
  fn verify(&self) -> String {
    let mut script = String::from(
      "set -eu\ncommand -v sha256sum >/dev/null || { echo 'Agent sync requires sha256sum on the SSH endpoint.' >&2; exit 1; }\nverify() {\n  [ -d \"$1\" ] && [ ! -L \"$1\" ] || return 1\n",
    );
    for file in self.files {
      let name = file.name;
      writeln!(
        script,
        "  [ -f \"$1/{name}\" ] && [ ! -L \"$1/{name}\" ] || return 1"
      )
      .unwrap();
      if !self.endpoint.target.is_windows() {
        writeln!(script, "  [ -x \"$1/{name}\" ] || return 1").unwrap();
      }
      writeln!(
        script,
        "  [ \"$(sha256sum < \"$1/{name}\")\" = '{}  -' ] || return 1",
        file.hash,
      )
      .unwrap();
    }
    script.push_str("}\n");
    writeln!(script, "dest={}", shell(&self.dir)).unwrap();
    script
  }

  fn check_script(&self) -> String {
    let mut script = self.verify();
    script.push_str(
      "if verify \"$dest\"; then printf 'present\\n'; elif [ -e \"$dest\" ] || [ -L \"$dest\" ]; then echo 'The cached ctream agent failed verification.' >&2; exit 1; else printf 'missing\\n'; fi\n",
    );
    script
  }

  fn upload_script(&self) -> String {
    let mut script = self.verify();
    writeln!(script, "store={}", shell(&self.endpoint.store)).unwrap();
    script.push_str(
      "umask 077\nmkdir -p -- \"$store\"\n[ ! -L \"$store\" ]\nstage=$(mktemp -d -- \"$store/.incoming.XXXXXXXXXX\")\ntrap 'rm -rf -- \"$stage\"' EXIT\ntrap 'exit 1' HUP INT TERM\n",
    );
    for file in self.files {
      writeln!(
        script,
        "head -c {} > \"$stage/{}\"",
        file.bytes.len(),
        file.name
      )
      .unwrap();
      if !self.endpoint.target.is_windows() {
        writeln!(script, "chmod 700 -- \"$stage/{}\"", file.name).unwrap();
      }
    }
    // The destination is immutable. If another viewer won the rename race,
    // accept the existing directory only after verifying the same inventory.
    script.push_str(
      "verify \"$stage\" || { echo 'The uploaded ctream agent failed verification.' >&2; exit 1; }\nif ! mv -T -- \"$stage\" \"$dest\" 2>/dev/null && ! verify \"$dest\"; then\n  echo 'Could not install the uploaded ctream agent.' >&2\n  exit 1\nfi\nprintf 'installed\\n'\n",
    );
    script
  }
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
  if cancelled.load(Ordering::Acquire) {
    Err(io::Error::new(
      io::ErrorKind::Interrupted,
      "Connection cancelled",
    ))
  } else {
    Ok(())
  }
}

fn read_bounded(mut reader: impl Read, limit: u64) -> io::Result<Vec<u8>> {
  let mut bytes = Vec::new();
  reader.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
  if bytes.len() as u64 > limit {
    return Err(io::Error::other(
      "ctream SSH response exceeded its size limit",
    ));
  }
  Ok(bytes)
}

fn wait_thread<T>(worker: thread::JoinHandle<io::Result<T>>) -> io::Result<T> {
  worker
    .join()
    .map_err(|_| io::Error::other("ctream SSH I/O worker failed"))?
}

fn read_diagnostics(mut reader: impl Read) -> io::Result<Vec<u8>> {
  const LIMIT: usize = 16 * 1024;
  let mut stderr = io::stderr();
  let show = stderr.is_terminal();
  let mut diagnostics = Vec::new();
  let mut buffer = [0; 4096];
  loop {
    let count = reader.read(&mut buffer)?;
    if count == 0 {
      return Ok(diagnostics);
    }
    if show {
      // The ctl SSH trust prompt can end without a newline and wait on /dev/tty.
      let _ = stderr.write_all(&buffer[..count]);
      let _ = stderr.flush();
    }
    diagnostics.extend_from_slice(&buffer[..count.min(LIMIT - diagnostics.len())]);
  }
}

fn run(
  mut command: Command,
  files: &[Artifact],
  limit: u64,
  timeout: Duration,
  cancelled: &AtomicBool,
) -> io::Result<Vec<u8>> {
  check_cancelled(cancelled)?;
  command
    .stdin(if files.is_empty() {
      Stdio::null()
    } else {
      Stdio::piped()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  let mut child = ChildProcess::spawn(command)?;
  let write = child.take_stdin().map(|mut stdin| {
    let payload: Vec<_> = files.iter().map(|file| Arc::clone(&file.bytes)).collect();
    thread::spawn(move || -> io::Result<()> {
      for bytes in payload {
        stdin.write_all(&bytes)?;
      }
      Ok(())
    })
  });
  let stdout = child.take_stdout().expect("piped stdout");
  let stderr = child.take_stderr().expect("piped stderr");
  let stdout = thread::spawn(move || read_bounded(stdout, limit));
  let stderr = thread::spawn(move || read_diagnostics(stderr));
  let started = Instant::now();
  let result = loop {
    if let Err(error) = check_cancelled(cancelled) {
      break Err(error);
    }
    if started.elapsed() >= timeout {
      break Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "ctream SSH command timed out",
      ));
    }
    match child.try_wait() {
      Ok(Some(exit))
        if write.as_ref().is_none_or(thread::JoinHandle::is_finished)
          && stdout.is_finished()
          && stderr.is_finished() =>
      {
        break Ok(exit);
      }
      Ok(_) => thread::sleep(Duration::from_millis(20)),
      Err(error) => break Err(error),
    }
  };
  let exit = match result {
    Ok(exit) => exit,
    Err(error) => {
      let _ = child.kill();
      let _ = child.wait();
      return Err(error);
    }
  };
  let error = wait_thread(stderr)?;
  if !exit.success() {
    let message = String::from_utf8_lossy(&error);
    return Err(io::Error::other(if message.trim().is_empty() {
      format!("ctream SSH command exited with {exit}")
    } else {
      message.trim().to_owned()
    }));
  }
  if let Some(write) = write {
    wait_thread(write)?;
  }
  wait_thread(stdout)
}

pub(super) fn connect(
  workspace: &Workspace,
  host: &Host,
  cancelled: &AtomicBool,
  status: impl Fn(&'static str),
) -> io::Result<SshCommand> {
  connect_using(
    workspace,
    host,
    &std::env::current_exe()?,
    cancelled,
    status,
  )
}

fn connect_using(
  workspace: &Workspace,
  host: &Host,
  executable: &Path,
  cancelled: &AtomicBool,
  status: impl Fn(&'static str),
) -> io::Result<SshCommand> {
  status("Connecting to host…");
  let endpoint = Endpoint::parse(&run(
    workspace.ssh(host, &probe_script()),
    &[],
    8192,
    COMMAND_TIMEOUT,
    cancelled,
  )?)?;
  status("Checking the agent…");
  let (local_agent, files) = artifacts(executable, endpoint.target)?;
  let install = Install {
    endpoint: &endpoint,
    dir: format!("{}/{}", endpoint.store, release_key(&files)),
    files: &files,
  };
  match run(
    workspace.ssh(host, &install.check_script()),
    &[],
    64,
    COMMAND_TIMEOUT,
    cancelled,
  )?
  .as_slice()
  {
    b"present\n" => {}
    b"missing\n" => {
      status("Syncing the agent…");
      let reply = run(
        workspace.ssh(host, &install.upload_script()),
        &files,
        64,
        TRANSFER_TIMEOUT,
        cancelled,
      )?;
      if reply != b"installed\n" {
        return Err(io::Error::other(
          "Unexpected ctream agent installation response",
        ));
      }
    }
    _ => return Err(io::Error::other("Unexpected ctream agent cache response")),
  }
  status("Starting the agent…");
  let agent = format!("{}/{}", install.dir, endpoint.target.agent());
  let catalog = run(
    workspace.ssh(
      host,
      &format!(
        "exec {} {} capture-drivers",
        shell(&agent),
        CAPTURE_CATALOG_VERSION
      ),
    ),
    &[],
    CATALOG_LIMIT,
    COMMAND_TIMEOUT,
    cancelled,
  )
  .map_err(|error| catalog_error(&local_agent, error))?;
  let catalog: Vec<CaptureDriverDescriptor> =
    serde_json::from_slice(&catalog).map_err(|error| {
      io::Error::other(format!(
        "The synchronized agent returned an invalid driver catalog: {error}"
      ))
    })?;
  if !capture_systems(endpoint.target)
    .iter()
    .any(|system| catalog.iter().any(|entry| entry.systems.contains(system)))
  {
    return Err(io::Error::other(
      "The synchronized agent does not support this display platform",
    ));
  }
  check_cancelled(cancelled)?;
  workspace.transport(host, &agent)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_probe_only_accepts_supported_targets_and_absolute_store_paths() {
    let endpoint = Endpoint::parse(
      b"ctream-sync-v1\nx86_64-pc-windows-msvc\n/mnt/c/Users/Example User/.tokn/ctl/ctream/agents\n",
    )
    .unwrap();
    assert_eq!(endpoint.target, Target::WindowsX64);
    assert_eq!(
      endpoint.store,
      "/mnt/c/Users/Example User/.tokn/ctl/ctream/agents"
    );
    for response in [
      b"ctream-sync-v1\nx86_64-apple-darwin\n/tmp/ctream\n".as_slice(),
      b"ctream-sync-v1\nx86_64-unknown-linux-musl\nrelative/path\n",
      b"ctream-sync-v1\nx86_64-unknown-linux-musl\n/valid\nextra\n",
      b"ctream-sync-v1\nx86_64-unknown-linux-musl\n/control\0path\n",
    ] {
      assert!(Endpoint::parse(response).is_err());
    }
  }

  #[test]
  fn packaging_location_precedes_development_builds() {
    let roots = artifact_directories(
      Path::new("repo/ctream-target/aarch64-apple-darwin/debug/ctream-preview"),
      Target::WindowsX64,
    );
    assert_eq!(
      roots.first().unwrap(),
      Path::new(
        "repo/ctream-target/aarch64-apple-darwin/debug/ctream-agents/x86_64-pc-windows-msvc"
      )
    );
    assert!(roots.contains(&PathBuf::from(
      "repo/ctream-target/x86_64-pc-windows-gnu/release"
    )));
    let roots = artifact_directories(
      Path::new("repo/ctream-target/debug/ctream-preview"),
      Target::WindowsX64,
    );
    assert!(roots.contains(&PathBuf::from(
      "repo/ctream-target/x86_64-pc-windows-gnu/release"
    )));
  }

  #[cfg(unix)]
  #[test]
  fn development_uses_the_newest_build_across_profiles_and_toolchains() {
    use std::time::UNIX_EPOCH;

    let scratch = Scratch::new();
    let preview = scratch.0.join("target/debug/ctream-preview");
    let [packaged, old, latest] = [
      "target/debug/ctream-agents/x86_64-pc-windows-msvc/ctream-agent.exe",
      "target/x86_64-pc-windows-msvc/release/ctream-agent.exe",
      "target/x86_64-pc-windows-gnu/debug/ctream-agent.exe",
    ]
    .map(|path| scratch.0.join(path));
    for (path, seconds) in [(&old, 10), (&latest, 20)] {
      fs::create_dir_all(path.parent().unwrap()).unwrap();
      let mut file = File::create(path).unwrap();
      file.write_all(b"agent").unwrap();
      file
        .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
    }
    assert_eq!(artifacts(&preview, Target::WindowsX64).unwrap().0, latest);

    fs::create_dir_all(packaged.parent().unwrap()).unwrap();
    fs::write(&packaged, b"same revision bundle").unwrap();
    assert_eq!(artifacts(&preview, Target::WindowsX64).unwrap().0, packaged);
  }

  #[cfg(unix)]
  #[test]
  fn managed_preview_rechecks_the_active_build_for_a_missing_agent() {
    use ctl_bundle::ctream::{
      BuildMetadata, install_development, native_target, viewer_executables,
    };
    use std::collections::BTreeMap;

    let scratch = Scratch::new();
    let root = scratch.0.join(".tokn/ctl/ctlup/store/ctream");
    let mut inputs: BTreeMap<_, _> = viewer_executables(native_target().unwrap())
      .into_iter()
      .map(|name| {
        let path = scratch.0.join(name);
        fs::write(&path, name).unwrap();
        (name.to_owned(), path)
      })
      .collect();
    let build = BuildMetadata {
      version: None,
      revision: None,
      built_at: None,
      channel: "local".into(),
    };
    let target = Target::LinuxX64;
    let original = install_development(&root, &inputs, &build).unwrap();
    let error = artifacts(&original.preview, target).err().unwrap();
    assert!(is_missing_agent(&error));

    let agent = scratch.0.join(target.agent());
    let helper = scratch.0.join("ctream-wayland-input");
    fs::write(&agent, b"new agent").unwrap();
    fs::write(&helper, b"matching helper").unwrap();
    inputs.insert(target.relative_path(target.agent()), agent.clone());
    inputs.insert(target.relative_path("ctream-wayland-input"), helper);
    let current = install_development(&root, &inputs, &build).unwrap();

    let (path, files) = artifacts(&original.preview, target).unwrap();
    assert_eq!(
      path,
      target.directory(&current.directory).join(target.agent())
    );
    assert_eq!(&*files[0].bytes, b"new agent");
    assert_eq!(&*files[1].bytes, b"matching helper");
    assert!(
      !target
        .directory(&original.directory)
        .join(target.agent())
        .exists()
    );

    fs::write(agent, b"another build").unwrap();
    install_development(&root, &inputs, &build).unwrap();
    // A viewer with its own agent keeps using that agent.
    let (path, _) = artifacts(&current.preview, target).unwrap();
    assert_eq!(
      path,
      target.directory(&current.directory).join(target.agent())
    );
  }

  #[test]
  fn a_legacy_agent_usage_error_explains_which_build_to_replace() {
    let error = catalog_error(
      Path::new("target/release/ctream-agent"),
      io::Error::other(r#"Error: "usage: ctream-agent VERSION OPERATION [DISPLAY [WINDOW]]""#),
    );
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    let message = error.to_string();
    assert!(message.contains("target/release/ctream-agent"));
    assert!(message.contains(CAPTURE_CATALOG_VERSION));
    assert!(message.contains("Rebuild ctream-agent"));
  }

  #[cfg(unix)]
  struct Scratch(PathBuf);

  #[cfg(unix)]
  impl Scratch {
    fn new() -> Self {
      use std::sync::atomic::AtomicU32;
      static NEXT: AtomicU32 = AtomicU32::new(0);
      let root = std::env::temp_dir().join(format!(
        "ctream sync '{}' {}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
      ));
      fs::create_dir(&root).unwrap();
      Self(root)
    }
  }

  #[cfg(unix)]
  impl Drop for Scratch {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  #[cfg(unix)]
  fn command(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
  }

  #[cfg(unix)]
  #[test]
  fn wsl_probe_uses_the_native_windows_architecture_and_data_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = Scratch::new();
    let mounted = scratch.0.join("Mounted System32");
    fs::create_dir(&mounted).unwrap();
    let mounted_powershell = mounted.join("powershell.exe");
    let wslpath = format!(
      r#"case "$3" in
  'C:\Users\Example User') printf '%s\n' '/mnt/c/Users/Example User' ;;
  'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe') printf '%s\n' {} ;;
  *) exit 1 ;;
esac"#,
      shell(mounted_powershell.to_str().unwrap()),
    );
    let scripts = [
      (
        "uname",
        "case \"$1\" in -s) echo Linux;; -r) echo '6.6.0-microsoft-standard';; -m) echo aarch64;; esac",
      ),
      (
        "powershell.exe",
        r#"case "$5" in
  *'GetFolderPath("UserProfile")'*) printf '%s\n%s' AMD64 'C:\Users\Example User' ;;
  *) exit 1 ;;
esac"#,
      ),
      ("wslpath", wslpath.as_str()),
    ];
    for (name, script) in scripts {
      let path = scratch.0.join(name);
      fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
      fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for use_mounted_fallback in [false, true] {
      if use_mounted_fallback {
        fs::rename(scratch.0.join("powershell.exe"), &mounted_powershell).unwrap();
      }
      let mut probe = command(&probe_script());
      probe.env(
        "PATH",
        std::env::join_paths([
          scratch.0.as_path(),
          Path::new("/usr/bin"),
          Path::new("/bin"),
        ])
        .unwrap(),
      );
      let endpoint =
        Endpoint::parse(&run(probe, &[], 8192, COMMAND_TIMEOUT, &AtomicBool::new(false)).unwrap())
          .unwrap();
      assert_eq!(endpoint.target, Target::WindowsX64);
      assert_eq!(
        endpoint.store,
        "/mnt/c/Users/Example User/.tokn/ctl/ctream/agents"
      );
    }
  }

  #[cfg(unix)]
  #[test]
  fn linux_probe_uses_the_ctl_data_directory_only_if_absolute() {
    use std::os::unix::fs::PermissionsExt as _;

    let scratch = Scratch::new();
    let uname = scratch.0.join("uname");
    fs::write(
      &uname,
      "#!/bin/sh\ncase \"$1\" in -s) echo Linux;; -r) echo 6.12.0;; -m) echo x86_64;; esac\n",
    )
    .unwrap();
    fs::set_permissions(uname, fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::join_paths([
      scratch.0.as_path(),
      Path::new("/usr/bin"),
      Path::new("/bin"),
    ])
    .unwrap();
    for (data_override, expected) in [
      (None, "/home/example/.tokn/ctl/ctream/agents"),
      (Some("relative"), "/home/example/.tokn/ctl/ctream/agents"),
      (Some("/srv/example ctl/"), "/srv/example ctl/ctream/agents"),
    ] {
      let mut probe = command(&probe_script());
      probe
        .env("PATH", &path)
        .env("HOME", "/home/example")
        .env_remove("CTL_DATA_DIR");
      if let Some(path) = data_override {
        probe.env("CTL_DATA_DIR", path);
      }
      let result = run(probe, &[], 8192, COMMAND_TIMEOUT, &AtomicBool::new(false)).unwrap();
      let endpoint = Endpoint::parse(&result).unwrap();
      assert_eq!(endpoint.target, Target::LinuxX64);
      assert_eq!(endpoint.store, expected);
    }
  }

  #[cfg(unix)]
  #[test]
  fn timeout_and_cancellation_return_without_waiting_for_the_remote_command() {
    let cancelled = AtomicBool::new(false);
    assert_eq!(
      run(
        command("exec sleep 5"),
        &[],
        64,
        Duration::from_millis(20),
        &cancelled,
      )
      .unwrap_err()
      .kind(),
      io::ErrorKind::TimedOut
    );
    cancelled.store(true, Ordering::Relaxed);
    assert_eq!(
      run(command("exit 1"), &[], 64, COMMAND_TIMEOUT, &cancelled)
        .unwrap_err()
        .kind(),
      io::ErrorKind::Interrupted
    );
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn upload_commits_only_the_complete_verified_file_set() {
    let scratch = Scratch::new();
    let endpoint = Endpoint {
      target: Target::LinuxX64,
      store: scratch.0.join("agents").to_str().unwrap().into(),
    };
    let files = [
      Artifact::new("ctream-agent", b"agent\0bytes\n".as_slice()),
      Artifact::new("ctream-wayland-input", b"helper\x01bytes\0".as_slice()),
    ];
    let install = Install {
      endpoint: &endpoint,
      dir: format!("{}/{}", endpoint.store, release_key(&files)),
      files: &files,
    };
    let cancelled = AtomicBool::new(false);
    let run_fixture =
      |script: &str, files| run(command(script), files, 64, COMMAND_TIMEOUT, &cancelled);
    assert_eq!(
      run_fixture(&install.check_script(), &[]).unwrap(),
      b"missing\n"
    );
    let upload = install.upload_script();
    assert!(run_fixture(&upload, &files[..1]).is_err());
    assert_eq!(fs::read_dir(&endpoint.store).unwrap().count(), 0);
    assert_eq!(run_fixture(&upload, &files).unwrap(), b"installed\n");
    assert_eq!(run_fixture(&upload, &files).unwrap(), b"installed\n");
    assert_eq!(
      run_fixture(&install.check_script(), &[]).unwrap(),
      b"present\n"
    );
    assert_eq!(fs::read_dir(&endpoint.store).unwrap().count(), 1);
    for file in &files {
      let path = Path::new(&install.dir).join(file.name);
      assert_eq!(fs::read(path).unwrap(), &*file.bytes);
    }
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn saved_host_connection_uploads_once_then_reuses_verified_agent() {
    use std::{cell::RefCell, os::unix::fs::PermissionsExt as _};
    let scratch = Scratch::new();
    let store = scratch.0.join("agents");
    let ctl = scratch.0.join("ctl");
    fs::write(
      &ctl,
      format!(
        r#"#!/bin/sh
[ "$1" = -H ] && [ "$2" = fixture-id ] && [ "$3" = --remote-platform ] &&
  [ "$4" = unix ] && [ "$5" = exec ] && [ "$6" = sh ] &&
  [ "$7" = -c ] && [ "$#" = 8 ] || exit 1
case "$8" in
  *ctream-sync-v1*) printf 'ctream-sync-v1\nx86_64-unknown-linux-musl\n%s\n' {};;
  *) exec /bin/sh -c "$8";;
esac
"#,
        shell(store.to_str().unwrap()),
      ),
    )
    .unwrap();
    fs::set_permissions(&ctl, fs::Permissions::from_mode(0o700)).unwrap();
    let agent_dir = scratch.0.join("ctream-agents/x86_64-unknown-linux-musl");
    fs::create_dir_all(&agent_dir).unwrap();
    fs::write(
      agent_dir.join("ctream-agent"),
      r#"#!/bin/sh
[ "$1" = 1.0.8 ] && [ "$2" = capture-drivers ] || exit 1
printf '%s' '[{"id":"mock","name":"Mock","scopes":["display"],"systems":["x11"]}]'
"#,
    )
    .unwrap();
    let workspace = Workspace::with_ctl(ctl);
    let host = Host {
      id: "fixture-id".into(),
      name: "Fixture".into(),
    };
    let cancelled = AtomicBool::new(false);
    for expected_sync in [true, false] {
      let statuses = RefCell::new(Vec::new());
      let transport = connect_using(
        &workspace,
        &host,
        &scratch.0.join("ctream-preview"),
        &cancelled,
        |status| statuses.borrow_mut().push(status),
      )
      .unwrap();
      let catalog = ctream_client::Client::new(transport)
        .capture_catalog()
        .unwrap();
      assert_eq!(catalog[0].info.id, "mock");
      assert_eq!(
        statuses.borrow().contains(&"Syncing the agent…"),
        expected_sync
      );
      assert_eq!(fs::read_dir(&store).unwrap().count(), 1);
    }
  }
}
