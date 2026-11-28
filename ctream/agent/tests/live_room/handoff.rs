//! Exercise real process, stdio and local IPC lifetimes without a display driver.
use super::{LiveAction, LiveEvent, Target, Watcher};
use interprocess::local_socket::{
  GenericFilePath, ToFsName, tokio::Stream, traits::tokio::Stream as _,
};
use serde_json::{Value, json};
use std::{
  fs,
  io::{self, Write},
  path::{Path, PathBuf},
  process::{Child, Command, Output, Stdio},
  time::{Duration, Instant},
};
use tokio::{
  io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
  runtime::Runtime,
  time::timeout,
};
use uuid::Uuid;

const LIMIT: Duration = Duration::from_secs(20);
const AGENT: &str = env!("CARGO_BIN_EXE_ctream-agent");

struct Daemon(Child);

impl Drop for Daemon {
  fn drop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}

struct Host {
  _temp: tempfile::TempDir,
  directory: PathBuf,
  endpoint: PathBuf,
  runtime: Runtime,
}

impl Host {
  fn new() -> io::Result<Self> {
    // Keep the Unix socket under the platform's sockaddr_un path limit.
    #[cfg(unix)]
    let temp = tempfile::tempdir_in("/tmp")?;
    #[cfg(windows)]
    let temp = tempfile::tempdir()?;
    let id = Uuid::new_v4();
    let directory = temp.path().join(id.to_string());
    #[cfg(unix)]
    {
      use std::os::unix::fs::DirBuilderExt as _;
      fs::DirBuilder::new().mode(0o700).create(&directory)?;
    }
    #[cfg(windows)]
    fs::create_dir(&directory)?;
    #[cfg(unix)]
    let endpoint = directory.join("v2.sock");
    #[cfg(windows)]
    let endpoint = PathBuf::from(format!(r"\\.\pipe\ctream-test-{id}"));
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()?;
    Ok(Self {
      _temp: temp,
      directory,
      endpoint,
      runtime,
    })
  }

  fn agent(&self, exe: &Path) -> Command {
    let mut cmd = Command::new(exe);
    cmd
      .env("CTREAM_TEST_DAEMON_RUNTIME", &self.directory)
      .env_remove("CTREAM_TEST_RECOVERY_DAEMON")
      .env_remove("CTREAM_INPUT_DELEGATION");
    cmd
  }

  fn cli(&self, args: &[&str]) -> io::Result<Output> {
    let mut cmd = tokio::process::Command::from(self.agent(AGENT.as_ref()));
    cmd.args(args).kill_on_drop(true);
    self
      .runtime
      .block_on(async { timeout(LIMIT, cmd.output()).await? })
  }

  fn status(&self) -> io::Result<Value> {
    let output = self.cli(&["daemon", "status"])?;
    if !output.status.success() {
      return Err(io::Error::other(
        String::from_utf8_lossy(&output.stderr).into_owned(),
      ));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
  }

  fn wait_for(&self, check: impl Fn(&Value) -> bool) -> io::Result<Value> {
    let deadline = Instant::now() + LIMIT;
    loop {
      let status = self.status();
      if let Ok(status) = &status
        && check(status)
      {
        return Ok(status.clone());
      }
      assert!(Instant::now() < deadline, "last daemon status: {status:?}");
      std::thread::sleep(Duration::from_millis(20));
    }
  }

  // A pre-handoff host-side client writes its application opening directly.
  fn private_request(&self, request: Value) -> io::Result<(BufReader<Stream>, Value)> {
    self.runtime.block_on(async {
      timeout(LIMIT, async {
        let socket =
          Stream::connect(self.endpoint.as_path().to_fs_name::<GenericFilePath>()?).await?;
        let mut stream = BufReader::new(socket);
        let reply = exchange(&mut stream, request).await?;
        Ok((stream, reply))
      })
      .await?
    })
  }
}

async fn exchange(stream: &mut BufReader<Stream>, request: Value) -> io::Result<Value> {
  let mut bytes = serde_json::to_vec(&request)?;
  bytes.push(b'\n');
  stream.get_mut().write_all(&bytes).await?;
  let mut reply = String::new();
  stream.read_line(&mut reply).await?;
  Ok(serde_json::from_str(&reply)?)
}

fn publish(watcher: &mut Watcher, action: LiveAction) -> io::Result<LiveEvent> {
  let input = watcher.child.stdin.as_mut().unwrap();
  writeln!(input, "{}", serde_json::to_string(&action)?)?;
  input.flush()?;
  Ok(LiveEvent::Action {
    action_id: None,
    participant_id: watcher.me.participant_id.clone(),
    action,
  })
}

#[test]
fn online_upgrade_preserves_two_live_viewers_and_recovers_an_abandoned_preparation()
-> io::Result<()> {
  let host = Host::new()?;
  assert_eq!(host.status()?["state"], "offline", "status must be passive");
  let mut old = Daemon(host.agent(AGENT.as_ref()).arg("daemon").spawn()?);
  let before = host.wait_for(|s| s["state"] == "running")?;
  assert_eq!(before["pid"], old.0.id());

  let target = Target::X11(12);
  let mut alice = Watcher::open_with(host.agent(AGENT.as_ref()), target, "x11-a", "viewer");
  alice.snapshot();
  let mut bob = Watcher::open_with(host.agent(AGENT.as_ref()), target, "x11-a", "viewer");
  assert!(bob.snapshot().contains(&alice.me));
  assert_eq!(
    alice.next(),
    LiveEvent::Joined {
      participant: bob.me.clone()
    }
  );

  let candidate = host
    .directory
    .join(format!("standby agent{}", std::env::consts::EXE_SUFFIX));
  fs::copy(AGENT, &candidate)?;
  let (legacy, reply) = host.private_request(json!({
    "room":{"target":target.to_string(),"window":"x11-legacy"},
    "participant":{"participant_id":Uuid::new_v4(),"agent_id":"legacy"},"subscribe":false
  }))?;
  assert_eq!(reply, true);
  let refused = host.cli(&["daemon", "upgrade", candidate.to_str().unwrap()])?;
  assert!(!refused.status.success());
  assert!(String::from_utf8_lossy(&refused.stderr).contains("legacy"));
  drop(legacy);
  host.wait_for(|s| s["legacy_connections"] == 0)?;

  let ticket = Uuid::new_v4();
  let (standby, prepared) = host
    .private_request(json!({"maintenance":{"operation":"upgrade","format":1,"ticket":ticket}}))?;
  assert_eq!(prepared, json!({"state":"prepared","ticket":ticket}));
  // The first standby goes away before it validates the checkpoint.
  drop(standby);
  let rollback = host.wait_for(|s| s["state"] == "running")?;
  assert_eq!(rollback["generation"], before["generation"]);
  assert!(rollback["selected_executable"].is_null());
  // Even after the candidate validates the checkpoint, failure to persist
  // the host selection must leave the original daemon and its peers live.
  let selector = host.directory.join("selected.json");
  fs::create_dir(&selector)?;
  let rejected = host.cli(&["daemon", "upgrade", candidate.to_str().unwrap()])?;
  assert!(!rejected.status.success());
  fs::remove_dir(selector)?;
  assert_eq!(host.status()?["generation"], before["generation"]);
  let action = LiveAction::Move {
    point: [0.25, 0.75],
  };
  let expected = publish(&mut alice, action)?;
  assert_eq!(bob.next(), expected);
  assert_eq!(alice.next(), expected);

  // Own the actual successor process so the fixture can stop it on every OS.
  let successor = Daemon(
    host
      .agent(&candidate)
      .args(["daemon", "standby"])
      .stdout(Stdio::null())
      .spawn()?,
  );
  let after = host.wait_for(|s| s["pid"] == successor.0.id())?;
  assert_ne!(after["generation"], before["generation"]);
  assert_ne!(after["pid"], before["pid"]);
  assert_eq!(
    fs::canonicalize(after["selected_executable"].as_str().unwrap())?,
    candidate.canonicalize()?
  );
  assert!(old.0.wait()?.success());

  publish(&mut alice, action)?;
  assert_eq!(
    bob.next(),
    expected,
    "the same viewer pipes must receive the original participant's next event"
  );
  assert_eq!(alice.next(), expected);
  bob.close();
  assert_eq!(
    alice.next(),
    LiveEvent::Left {
      participant_id: bob.me.participant_id.clone()
    }
  );
  alice.close();
  drop(successor);
  let stopped = host.status()?;
  assert_eq!(stopped["state"], "offline");
  assert_eq!(stopped["selected_executable"], after["selected_executable"]);
  Ok(())
}

// The real relay launches the selected executable with the argument "daemon".
// In this test executable that selects this helper through the Rust harness.
// A parent-owned file lock gives the test control of the detached process even
// if an assertion unwinds before its PID is known.
#[test]
fn recovery_daemon() -> io::Result<()> {
  if std::env::var("CTREAM_TEST_RECOVERY_DAEMON").as_deref() != Ok("1") {
    return Ok(());
  }
  let runtime = std::env::var_os("CTREAM_TEST_DAEMON_RUNTIME").unwrap();
  let parent = fs::File::open(Path::new(&runtime).join("rescue-parent.lock"))?;
  std::thread::spawn(move || {
    let result = parent.lock();
    std::process::exit(if result.is_ok() { 0 } else { 1 });
  });
  ctream_agent::daemon::run_daemon()
}

#[test]
fn online_upgrade_rescues_a_committed_image_once_if_the_standby_dies() -> io::Result<()> {
  let host = Host::new()?;
  let parent = fs::File::create_new(host.directory.join("rescue-parent.lock"))?;
  parent.lock()?;
  let mut desktop = host.agent(AGENT.as_ref());
  #[cfg(target_os = "linux")]
  desktop.env("XAUTHORITY", host.directory.join("desktop.authority"));
  let original = Daemon(desktop.arg("daemon").spawn()?);
  host.wait_for(|s| s["pid"] == original.0.id())?;
  #[cfg(target_os = "linux")]
  let assert_authority = |name: &str| -> io::Result<()> {
    let (_, context) =
      host.private_request(json!({"maintenance":{"operation":"launch_context"}}))?;
    assert_eq!(
      context["XAUTHORITY"],
      serde_json::to_value(host.directory.join(name).into_os_string())?
    );
    Ok(())
  };
  let open_viewer = || {
    let mut cmd = host.agent(AGENT.as_ref());
    cmd.env("CTREAM_TEST_RECOVERY_DAEMON", "1");
    #[cfg(target_os = "linux")]
    cmd.env("XAUTHORITY", host.directory.join("relay.authority"));
    Watcher::open_with(cmd, Target::X11(12), "x11-a", "viewer")
  };
  let mut viewer = open_viewer();
  viewer.snapshot();

  let ticket = Uuid::new_v4();
  let selected = std::env::current_exe()?.canonicalize()?;
  let (mut standby, prepared) = host
    .private_request(json!({"maintenance":{"operation":"upgrade","format":1,"ticket":ticket}}))?;
  assert_eq!(prepared, json!({"state":"prepared","ticket":ticket}));
  let committed = host.runtime.block_on(async {
    timeout(
      LIMIT,
      exchange(
        &mut standby,
        json!({"state":"validated","ticket":ticket,"executable":selected}),
      ),
    )
    .await?
  })?;
  assert_eq!(committed, json!({"state":"committed","ticket":ticket}));
  drop(standby); // Simulate the standby dying before it ever binds the endpoint.

  let recovered =
    host.wait_for(|s| s["generation"] == ticket.to_string() && s["resuming_lanes"] == 0)?;
  #[cfg(target_os = "linux")]
  assert_authority("desktop.authority")?;
  assert_ne!(recovered["pid"], original.0.id());
  assert_eq!(
    Path::new(recovered["executable"].as_str().unwrap()).canonicalize()?,
    selected
  );
  assert!(
    !host
      .directory
      .join(format!("handoff-{ticket}.json"))
      .exists()
  );
  let expected = publish(
    &mut viewer,
    LiveAction::Click {
      point: [0.125, 0.625],
    },
  )?;
  assert_eq!(viewer.next(), expected);
  let original_identity = viewer.me.participant_id.clone();
  viewer.close();

  parent.unlock()?;
  host.wait_for(|s| s["state"] == "offline")?;
  parent.lock()?;
  // A client using the original binary cold-starts the selected generation.
  // The consumed snapshot must never resurrect participants on this path.
  let mut cold = open_viewer();
  let roster = cold.snapshot();
  assert_eq!(roster, [cold.me.clone()]);
  assert_ne!(cold.me.participant_id, original_identity);
  let fresh = host.status()?;
  assert_ne!(fresh["generation"], recovered["generation"]);
  assert_eq!(fresh["executable"], recovered["executable"]);
  assert_eq!(fresh["resuming_lanes"], 0);
  #[cfg(target_os = "linux")]
  assert_authority("relay.authority")?;
  cold.close();
  parent.unlock()?;
  host.wait_for(|s| s["state"] == "offline")?;
  Ok(())
}
