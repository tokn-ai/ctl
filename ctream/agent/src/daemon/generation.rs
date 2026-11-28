//! Host-side selection and the one-shot standby handshake. The successor owns
//! the upgrade reservation while the old process still owns desktop input.
use super::context::LaunchContext;
use super::{CONTROL_LIMIT, Opening, endpoint, read_json, runtime, server, write_json};
use serde::{Deserialize, Serialize};
use std::{
  fs,
  io::{self, BufRead as _, Write as _},
  path::{Path, PathBuf},
  process::{Command, Stdio},
  time::{Duration, SystemTime},
};
use tokio::{io::BufReader, time::timeout};
use uuid::Uuid;

pub(super) const FORMAT: u32 = 1;
pub(super) const HANDOFF_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Maintenance {
  Status,
  LaunchContext,
  Upgrade { format: u32, ticket: Uuid },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Response {
  Prepared { ticket: Uuid },
  Validated { ticket: Uuid, executable: PathBuf },
  Committed { ticket: Uuid },
  Error { message: String },
}

pub(super) fn checkpoint_path(ticket: Uuid) -> io::Result<PathBuf> {
  Ok(endpoint::directory()?.join(format!("handoff-{ticket}.json")))
}

#[derive(Serialize, Deserialize)]
struct Decision {
  ticket: Uuid,
  executable: PathBuf,
  committed_at: SystemTime,
}

pub(super) async fn recover_disconnected() -> io::Result<()> {
  if decision()?.is_some() {
    let lock = endpoint::lock("upgrade.lock")?;
    if lock.try_lock_shared().is_ok() && decision()?.is_some() {
      // The standby died after commit. The durable decision selects the only
      // executable allowed to restore this one-shot checkpoint.
      drop(endpoint::connect_via(None).await?);
    }
  }
  Ok(())
}

fn read_selection() -> io::Result<Option<Decision>> {
  match selected_path(false).and_then(|p| crate::private_fs::private_options().read(true).open(p)) {
    Ok(file) => {
      crate::private_fs::check(&file.metadata()?, false, true)?;
      let selected: Decision = serde_json::from_reader(file)?;
      if !selected.executable.is_absolute() {
        return Err(io::Error::other("invalid selected daemon executable"));
      }
      Ok(Some(selected))
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error),
  }
}

fn decision() -> io::Result<Option<Decision>> {
  let Some(selected) = read_selection()? else {
    return Ok(None);
  };
  Ok(
    checkpoint_path(selected.ticket)?
      .try_exists()?
      .then_some(selected),
  )
}

/// A selected build can survive many logins. Only an outstanding, recent
/// handoff can carry the predecessor's desktop locators into a recovery spawn.
pub(super) fn selected_launch() -> io::Result<Option<(PathBuf, Option<LaunchContext>)>> {
  let Some(selected) = read_selection()? else {
    return Ok(None);
  };
  let context = if selected
    .committed_at
    .elapsed()
    .is_ok_and(|age| age <= server::handoff::RESUME_GRACE)
  {
    match server::handoff::launch_context(selected.ticket) {
      Ok(context) => Some(context),
      Err(error) if error.kind() == io::ErrorKind::NotFound => None,
      Err(error) => return Err(error),
    }
  } else {
    None
  };
  Ok(Some((selected.executable, context)))
}

async fn launch_context() -> io::Result<Option<LaunchContext>> {
  let socket = match endpoint::connect_existing().await {
    Ok(socket) => socket,
    Err(error) if endpoint::offline(&error) => {
      return Ok(selected_launch()?.and_then(|(_, context)| context));
    }
    Err(error) => return Err(error),
  };
  let mut stream = BufReader::new(socket);
  write_json(
    stream.get_mut(),
    &Opening::Maintenance {
      maintenance: Maintenance::LaunchContext,
    },
  )
  .await?;
  let context = timeout(
    super::IO_TIMEOUT,
    read_json(&mut stream, super::context::LIMIT),
  )
  .await??
  .ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::Unsupported,
      "running daemon cannot hand off; let its existing sessions finish",
    )
  })?;
  Ok(Some(context))
}

// Changing the selected build *is* the commit point. A failure to write the
// selector leaves the old daemon serving. The ticket only authorizes one resume
// while its matching private checkpoint exists.
pub(super) fn record_commit(ticket: Uuid, executable: PathBuf) -> io::Result<()> {
  let result = store(
    &selected_path(true)?,
    &Decision {
      ticket,
      executable,
      committed_at: SystemTime::now(),
    },
  );
  // Once the rename is visible the successor can observe the decision, even
  // if syncing the directory reports a delayed filesystem error.
  match result {
    Err(_) if decision()?.is_some_and(|decision| decision.ticket == ticket) => Ok(()),
    result => result,
  }
}

pub(super) fn discard_unused_images() -> io::Result<()> {
  // Called only while holding both election and startup locks, after any
  // committed image was consumed. An uncommitted image can survive SIGKILL.
  for entry in fs::read_dir(endpoint::directory()?)?.flatten() {
    if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
      continue;
    }
    let name = entry.file_name();
    if name
      .to_str()
      .and_then(|name| name.strip_prefix("handoff-"))
      .and_then(|name| name.strip_suffix(".json"))
      .is_some_and(|id| Uuid::try_parse(id).is_ok())
    {
      let _ = fs::remove_file(entry.path());
    }
  }
  Ok(())
}

fn finish_activation(ticket: Uuid) -> io::Result<()> {
  // The caller holds the permanent desktop election. Consume the image before
  // serving a single command so a later crash cannot resurrect a previous turn.
  fs::remove_file(checkpoint_path(ticket)?)?;
  #[cfg(unix)]
  fs::File::open(endpoint::directory()?)?.sync_all()?;
  Ok(())
}

pub(super) fn recover() -> io::Result<Option<server::handoff::Generation>> {
  let Some(decision) = decision()? else {
    return Ok(None);
  };
  if std::env::current_exe()?.canonicalize()? != decision.executable.canonicalize()? {
    return Err(io::Error::other(
      "pending desktop handoff belongs to a different executable",
    ));
  }
  // Only relays from this short, planned transition can still be waiting.
  // A later cold start must not resurrect yesterday's participants or turns.
  let restored = if decision
    .committed_at
    .elapsed()
    .is_ok_and(|age| age <= server::handoff::RESUME_GRACE)
  {
    Some(server::handoff::restore(decision.ticket)?)
  } else {
    None
  };
  finish_activation(decision.ticket)?;
  Ok(restored)
}

fn selected_path(create: bool) -> io::Result<PathBuf> {
  #[cfg(any(test, all(debug_assertions, feature = "test-fixtures")))]
  if let Some(dir) = std::env::var_os("CTREAM_TEST_DAEMON_RUNTIME") {
    return Ok(PathBuf::from(dir).join("selected.json"));
  }
  #[cfg(windows)]
  let directory = {
    let _ = create;
    endpoint::directory()?
  };
  #[cfg(unix)]
  let directory = {
    // SSH and GUI launchers can set different HOME/CTL_DATA_DIR values.
    // Their elected daemon and its cold-start selection belong to the OS user.
    use nix::unistd::{Uid, User};
    match User::from_uid(Uid::effective())? {
      Some(user) if user.dir.is_absolute() => {
        if !create {
          // With no selection, ordinary on-demand startup needs no storage.
          fs::symlink_metadata(user.dir.join(".tokn/ctl/ctream/daemon/selected.json"))?;
        }
        crate::private_fs::prepare(
          &user.dir,
          &[
            (".tokn", false),
            ("ctl", false),
            ("ctream", false),
            ("daemon", true),
          ],
          create,
        )?
      }
      _ => endpoint::directory()?,
    }
  };
  Ok(directory.join("selected.json"))
}

pub(super) fn selection() -> io::Result<Option<PathBuf>> {
  Ok(read_selection()?.map(|choice| choice.executable))
}

pub(super) fn store(path: &Path, value: &impl Serialize) -> io::Result<()> {
  let dir = path
    .parent()
    .ok_or_else(|| io::Error::other("missing daemon directory"))?;
  let mut builder = fs::DirBuilder::new();
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt as _;
    builder.mode(0o700);
  }
  builder.recursive(true).create(dir)?;
  let mut temp = tempfile::NamedTempFile::new_in(dir)?;
  serde_json::to_writer(temp.as_file_mut(), value)?;
  temp.as_file().sync_all()?;
  temp.persist(path).map_err(|e| e.error)?;
  #[cfg(unix)]
  fs::File::open(dir)?.sync_all()?;
  Ok(())
}

pub fn status() -> io::Result<()> {
  let value = runtime()?.block_on(async {
    let socket = match endpoint::connect_existing().await {
      Ok(socket) => socket,
      Err(error) if endpoint::offline(&error) => return Ok(serde_json::json!({"state":"offline"})),
      Err(error) => return Err(error),
    };
    let mut stream = BufReader::new(socket);
    write_json(
      stream.get_mut(),
      &Opening::Maintenance {
        maintenance: Maintenance::Status,
      },
    )
    .await?;
    timeout(super::IO_TIMEOUT, read_json(&mut stream, CONTROL_LIMIT))
      .await??
      .ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::Unsupported,
          "running daemon predates lifecycle status",
        )
      })
  })?;
  let mut value: serde_json::Value = value;
  value["selected_executable"] = serde_json::to_value(selection()?)?;
  println!("{}", serde_json::to_string(&value)?);
  Ok(())
}

pub fn upgrade(candidate: Option<&str>) -> io::Result<()> {
  let executable = match candidate {
    Some(path) => fs::canonicalize(path)?,
    None => std::env::current_exe()?,
  };
  let mut command = Command::new(executable);
  if let Some(context) = runtime()?.block_on(launch_context())? {
    context.apply(&mut command)?;
  }
  command
    .args(["daemon", "standby"])
    .env_remove("CTREAM_INPUT_DELEGATION")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
  let mut standby = endpoint::detached(&mut command)?;
  let stdout = standby.stdout.take().unwrap();
  let mut line = String::new();
  std::io::BufReader::new(stdout).read_line(&mut line)?;
  if line != "active\n" {
    let message = serde_json::from_str::<String>(&line)
      .unwrap_or_else(|_| "candidate does not implement online upgrades".into());
    let status = standby.wait()?;
    return Err(io::Error::other(format!(
      "successor did not activate ({status}): {message}"
    )));
  }
  // A long-lived parent embedding the command must also reap the detached child.
  std::thread::Builder::new()
    .name("ctream-upgrade-exit".into())
    .spawn(move || {
      let _ = standby.wait();
    })?;
  Ok(())
}

pub fn standby() -> io::Result<()> {
  super::detach_session()?;
  runtime()?.block_on(async {
    let reservation = endpoint::lock("upgrade.lock")?;
    reservation
      .try_lock()
      .map_err(|_| io::Error::other("another daemon start or upgrade is in progress"))?;
    let ticket = Uuid::new_v4();
    let executable = std::env::current_exe()?;
    let prepared = match endpoint::connect_existing().await {
      Ok(socket) => {
        let mut stream = BufReader::new(socket);
        write_json(
          stream.get_mut(),
          &Opening::Maintenance {
            maintenance: Maintenance::Upgrade {
              format: FORMAT,
              ticket,
            },
          },
        )
        .await?;
        let read = |message| io::Error::new(io::ErrorKind::Unsupported, message);
        match timeout(HANDOFF_TIMEOUT, read_json(&mut stream, CONTROL_LIMIT)).await?? {
          Some(Response::Prepared { ticket: id }) if id == ticket => {}
          Some(Response::Error { message }) => return Err(io::Error::other(message)),
          _ => {
            return Err(read(
              "running daemon cannot hand off; let its existing sessions finish",
            ));
          }
        }
        let restored = server::handoff::restore(ticket)?;
        write_json(
          stream.get_mut(),
          &Response::Validated {
            ticket,
            executable: executable.clone(),
          },
        )
        .await?;
        match timeout(HANDOFF_TIMEOUT, read_json(&mut stream, CONTROL_LIMIT)).await {
          Ok(Ok(Some(Response::Committed { ticket: id }))) if id == ticket => {}
          Ok(Ok(Some(Response::Error { message }))) => return Err(io::Error::other(message)),
          _ if decision()?.is_some_and(|d| d.ticket == ticket && d.executable == executable) => {}
          _ => return Err(io::Error::other("predecessor did not commit the handoff")),
        }
        Some(restored)
      }
      Err(error) if endpoint::offline(&error) => None,
      Err(error) => return Err(error),
    };
    // The old process sends its final receipts and SWITCH frames before
    // releasing the sole endpoint. Windows also waits for its pipe instances.
    let endpoint = timeout(HANDOFF_TIMEOUT, async {
      loop {
        match endpoint::bind() {
          Ok(endpoint) => return Ok(endpoint),
          Err(error)
            if matches!(
              error.kind(),
              io::ErrorKind::WouldBlock
                | io::ErrorKind::PermissionDenied
                | io::ErrorKind::AddrInUse
            ) =>
          {
            tokio::time::sleep(Duration::from_millis(15)).await
          }
          Err(error) => return Err(error),
        }
      }
    })
    .await??;
    endpoint::wait_for_old_gestures()?;
    let state = if let Some(prepared) = prepared {
      finish_activation(ticket)?;
      prepared
    } else if let Some(restored) = recover()? {
      restored
    } else {
      record_commit(ticket, executable)?;
      server::handoff::Generation::default()
    };
    let _ = discard_unused_images();
    drop(reservation);
    // The operator may have disconnected after the commit. The daemon still
    // owns its desktop and must keep serving the original sessions.
    let _ = writeln!(io::stdout().lock(), "active");
    server::serve_generation(endpoint, state).await
  })
}
