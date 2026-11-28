//! A generation keeps admitting work until a verified successor asks it to
//! drain. The same-user standby validates the checkpoint before commit.
use super::{
  CONTROL_LIMIT, IO_TIMEOUT, Opening, Rooms, checkpoint, invalid,
  lane::{Gate, LaneState, Phase},
  read_json, serve_opened, write_json,
};
use crate::daemon::{
  context::LaunchContext,
  generation::{self, FORMAT, HANDOFF_TIMEOUT, Maintenance, Response},
  transport,
};
use serde::{Deserialize, Serialize};
use std::{
  collections::{BTreeMap, BTreeSet},
  fs, io,
  sync::{Arc, Mutex},
  time::Duration,
};
use tokio::{
  io::{AsyncBufRead, AsyncRead, AsyncReadExt as _, AsyncWrite, BufReader},
  sync::watch,
  time::timeout,
};
use uuid::Uuid;

pub(in crate::daemon) const RESUME_GRACE: Duration = Duration::from_secs(25);
const MAX_CHECKPOINT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedLane {
  opening: Opening,
  state: LaneState,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
  format: u32,
  ticket: Uuid,
  journal_base: std::path::PathBuf,
  launch_context: LaunchContext,
  desktop: checkpoint::Checkpoint,
  lanes: BTreeMap<Uuid, SavedLane>,
}

#[derive(Default)]
struct Registry {
  active: BTreeMap<Uuid, (Opening, Gate)>,
  dormant: BTreeMap<Uuid, SavedLane>,
  starting: BTreeSet<Uuid>,
  legacy: usize,
  delegates: usize,
  paused: bool,
  delegates_frozen: bool,
}

impl Registry {
  fn full(&self) -> bool {
    self.active.len() + self.dormant.len() + self.starting.len() + self.legacy + self.delegates
      >= super::MAX_CONNECTIONS
  }
}

pub(in crate::daemon) struct Generation {
  pub(super) rooms: Rooms,
  generation: Uuid,
  registry: Mutex<Registry>,
  committed: watch::Sender<bool>,
  last_session: Mutex<tokio::time::Instant>,
}

impl Default for Generation {
  fn default() -> Self {
    Self {
      rooms: Rooms::default(),
      generation: Uuid::new_v4(),
      registry: Mutex::default(),
      committed: watch::channel(false).0,
      last_session: Mutex::new(tokio::time::Instant::now()),
    }
  }
}

impl Drop for Generation {
  fn drop(&mut self) {
    checkpoint::abandon_all(&self.rooms);
  }
}

impl Generation {
  pub(super) fn idle_deadline(&self) -> tokio::time::Instant {
    *self.last_session.lock().unwrap() + super::IDLE_EXIT
  }

  fn expire_dormant(&self) {
    let lanes = std::mem::take(&mut self.registry.lock().unwrap().dormant);
    for (id, _) in lanes {
      checkpoint::abandon(&self.rooms, id);
    }
  }

  pub(super) async fn commit_signal(&self) {
    let mut rx = self.committed.subscribe();
    let _ = rx.wait_for(|commit| *commit).await;
  }

  pub(super) fn start_expiry(self: &Arc<Self>) {
    if self.registry.lock().unwrap().dormant.is_empty() {
      return;
    }
    let weak = Arc::downgrade(self);
    tokio::spawn(async move {
      tokio::time::sleep(RESUME_GRACE).await;
      if let Some(state) = weak.upgrade() {
        state.expire_dormant();
      }
    });
  }

  fn status(&self) -> io::Result<serde_json::Value> {
    let registry = self.registry.lock().unwrap();
    let desktop = self.rooms.state.lock().unwrap();
    let participants: BTreeSet<_> = desktop
      .rooms
      .values()
      .flat_map(|members| members.keys())
      .collect();
    let turn_holders: BTreeMap<_, _> = desktop
      .rooms
      .keys()
      .filter_map(|room| match desktop.turn_event(room) {
        ctream_proto::live::LiveEvent::Turn {
          holder: Some(holder),
          expires_at_ms,
        } => Some((holder.participant_id, expires_at_ms)),
        _ => None,
      })
      .collect();
    Ok(serde_json::json!({
      "state": if registry.paused {"draining"} else {"running"}, "format": FORMAT,
      "generation": self.generation, "pid": std::process::id(),
      "participants":participants.len(), "controllers": desktop.controllers.len(), "turn_holders":turn_holders,
      "executable": std::env::current_exe()?,
      "active_lanes": registry.active.len(), "resuming_lanes":registry.dormant.len(),
      "legacy_connections": registry.legacy,
    }))
  }
}

fn read_image(ticket: Uuid) -> io::Result<Image> {
  let file = crate::private_fs::private_options()
    .read(true)
    .open(generation::checkpoint_path(ticket)?)?;
  let metadata = file.metadata()?;
  crate::private_fs::check(&metadata, false, true)?;
  if metadata.len() > MAX_CHECKPOINT_BYTES {
    return Err(invalid("desktop checkpoint too large"));
  }
  let image: Image = serde_json::from_reader(io::BufReader::new(file))?;
  if image.format != FORMAT
    || image.ticket != ticket
    || !image.journal_base.is_absolute()
    || image.lanes.len() > super::MAX_CONNECTIONS
  {
    return Err(invalid("incompatible desktop checkpoint"));
  }
  image
    .desktop
    .validate_openings(image.lanes.iter().map(|(id, lane)| (*id, &lane.opening)))?;
  Ok(image)
}

pub(in crate::daemon) fn launch_context(ticket: Uuid) -> io::Result<LaunchContext> {
  Ok(read_image(ticket)?.launch_context)
}

pub(in crate::daemon) fn restore(ticket: Uuid) -> io::Result<Generation> {
  let image = read_image(ticket)?;
  if image.launch_context != LaunchContext::current() {
    return Err(invalid("standby has a different desktop launch context"));
  }
  let mut state = Generation::default();
  state.generation = ticket;
  state.rooms = Arc::new(super::Desktop {
    journal: crate::activity::Store::at(image.journal_base),
    ..Default::default()
  });
  state.registry.get_mut().unwrap().dormant = image.lanes;
  image.desktop.restore(&state.rooms)?;
  Ok(state)
}

struct Cutover<'a> {
  generation: &'a Generation,
  lanes: Vec<(Uuid, Opening, Gate)>,
  ticket: Uuid,
  committed: bool,
}

impl Drop for Cutover<'_> {
  fn drop(&mut self) {
    if !self.committed {
      checkpoint::pause_expiry(&self.generation.rooms, false);
      for (_, _, gate) in &self.lanes {
        gate.set_phase(Phase::Running);
      }
      let mut registry = self.generation.registry.lock().unwrap();
      registry.paused = false;
      registry.delegates_frozen = false;
    }
    if !self.committed
      && let Ok(path) = generation::checkpoint_path(self.ticket)
    {
      let _ = fs::remove_file(path);
    }
  }
}

async fn upgrade(
  generation: &Generation,
  ticket: Uuid,
  read: &mut (impl AsyncBufRead + Unpin),
  write: &mut (impl AsyncWrite + Unpin),
) -> io::Result<()> {
  let mut cutover = {
    let mut registry = generation.registry.lock().unwrap();
    if registry.paused
      || !registry.starting.is_empty()
      || registry.legacy > 0
      || !registry.dormant.is_empty()
    {
      return Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "desktop has legacy, joining or resuming clients; retry after they finish",
      ));
    }
    registry.paused = true;
    Cutover {
      generation,
      ticket,
      lanes: registry
        .active
        .iter()
        .map(|(id, (opening, gate))| (*id, opening.clone(), gate.clone()))
        .collect(),
      committed: false,
    }
  };
  let executable = timeout(HANDOFF_TIMEOUT, async {
    let mut lanes = BTreeMap::new();
    for phase in [Phase::DrainInput, Phase::Freeze] {
      if phase == Phase::Freeze {
        generation.registry.lock().unwrap().delegates_frozen = true;
        checkpoint::pause_expiry(&generation.rooms, true);
      }
      for (_, _, gate) in &cutover.lanes {
        gate.set_phase(phase);
      }
      for (id, opening, gate) in &cutover.lanes {
        match gate.wait_paused(phase).await {
          Ok(state) if phase == Phase::Freeze => {
            lanes.insert(
              *id,
              SavedLane {
                opening: opening.clone(),
                state,
              },
            );
          }
          Ok(_) => {}
          Err(error) => return Err(error),
        }
      }
    }
    // Platform helper delegates own cross-process input pins. A controller
    // releases idle helpers at its barrier; active gestures must finish first.
    while generation.registry.lock().unwrap().delegates != 0 {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let image = Image {
      format: FORMAT,
      ticket,
      journal_base: generation.rooms.journal.base()?,
      launch_context: LaunchContext::current(),
      desktop: checkpoint::Checkpoint::capture(&generation.rooms)?,
      lanes,
    };
    generation::store(&generation::checkpoint_path(ticket)?, &image)?;
    write_json(write, &Response::Prepared { ticket }).await?;
    match read_json(read, CONTROL_LIMIT).await? {
      Some(Response::Validated {
        ticket: other,
        executable,
      }) if ticket == other && executable.is_absolute() => Ok(executable),
      _ => Err(invalid("standby failed checkpoint validation")),
    }
  })
  .await
  .map_err(|_| {
    io::Error::new(
      io::ErrorKind::TimedOut,
      "could not reach a safe desktop handoff; current daemon resumed",
    )
  })??;

  if generation.registry.lock().unwrap().active.len() != cutover.lanes.len() {
    return Err(invalid("a participant left during handoff; retry"));
  }
  generation::record_commit(ticket, executable)?;
  cutover.committed = true;
  for (_, _, gate) in &cutover.lanes {
    gate.set_phase(Phase::Commit);
  }
  generation.committed.send_replace(true);
  write_json(write, &Response::Committed { ticket }).await?;
  Ok(())
}

// The Windows local-socket wrapper does not flush on AsyncWrite::flush.
// A management caller closes after reading its terminal reply. This also
// bounds the lifetime of a peer that stops reading before a cutover.
async fn peer_closed(reader: &mut (impl AsyncRead + Unpin)) {
  let _ = timeout(IO_TIMEOUT, reader.read(&mut [0u8])).await;
}

enum GuardKind {
  Starting(Uuid),
  Active(Uuid, Gate),
  Legacy,
  Delegate,
}

struct Guard<'a> {
  generation: &'a Generation,
  kind: GuardKind,
}
impl Drop for Guard<'_> {
  fn drop(&mut self) {
    *self.generation.last_session.lock().unwrap() = tokio::time::Instant::now();
    let mut reg = self.generation.registry.lock().unwrap();
    match &self.kind {
      GuardKind::Starting(id) => {
        reg.starting.remove(id);
        checkpoint::abandon(&self.generation.rooms, *id);
      }
      GuardKind::Active(id, gate) => {
        gate.finish();
        reg.active.remove(id);
        checkpoint::abandon(&self.generation.rooms, *id);
      }
      GuardKind::Legacy => reg.legacy -= 1,
      GuardKind::Delegate => reg.delegates -= 1,
    }
  }
}

pub(super) async fn route(
  stream: impl AsyncRead + AsyncWrite + Unpin,
  generation: &Generation,
) -> io::Result<()> {
  let (read, mut write) = tokio::io::split(stream);
  let mut read = BufReader::new(read);
  let Some(opening) = timeout(IO_TIMEOUT, read_json::<Opening>(&mut read, CONTROL_LIMIT)).await??
  else {
    return Ok(());
  };
  let (lane, resume) = match opening {
    Opening::Maintenance {
      maintenance: Maintenance::LaunchContext,
    } => {
      write_json(&mut write, &LaunchContext::current()).await?;
      peer_closed(&mut read).await;
      return Ok(());
    }
    Opening::Maintenance {
      maintenance: Maintenance::Status,
    } => {
      write_json(&mut write, &generation.status()?).await?;
      peer_closed(&mut read).await;
      return Ok(());
    }
    Opening::Maintenance {
      maintenance: Maintenance::Upgrade { format, ticket },
    } => {
      let result = if format == FORMAT {
        upgrade(generation, ticket, &mut read, &mut write).await
      } else {
        Err(invalid("incompatible standby format"))
      };
      if let Err(error) = result {
        write_json(
          &mut write,
          &Response::Error {
            message: error.to_string(),
          },
        )
        .await?;
      }
      peer_closed(&mut read).await;
      return Ok(());
    }
    Opening::Lane {
      format: FORMAT,
      lane,
      resume,
    } => (lane, resume),
    Opening::Lane { .. } => {
      write_json(&mut write, &"unsupported").await?;
      peer_closed(&mut read).await;
      return Ok(());
    }
    app => {
      let kind = {
        let mut registry = generation.registry.lock().unwrap();
        if registry.full() {
          return Err(invalid("desktop connection limit reached"));
        }
        if matches!(app, Opening::Delegate { .. }) && !registry.delegates_frozen {
          registry.delegates += 1;
          GuardKind::Delegate
        } else if !registry.paused {
          registry.legacy += 1;
          GuardKind::Legacy
        } else {
          return Err(invalid("desktop handoff in progress"));
        }
      };
      let _guard = Guard { generation, kind };
      return serve_opened(
        app,
        generation.rooms.clone(),
        &mut read,
        &mut write,
        &Gate::legacy(),
      )
      .await;
    }
  };
  let accepted = {
    let mut registry = generation.registry.lock().unwrap();
    if registry.paused {
      Err("pending")
    } else if !resume && registry.full() {
      Err("full")
    } else if registry.starting.contains(&lane)
      || registry.active.contains_key(&lane)
      || (resume != registry.dormant.contains_key(&lane))
    {
      Err("gone")
    } else {
      registry.starting.insert(lane);
      Ok(registry.dormant.remove(&lane))
    }
  };
  let saved = match accepted {
    Err(reply) => {
      write_json(&mut write, &reply).await?;
      peer_closed(&mut read).await;
      return Ok(());
    }
    Ok(saved) => saved,
  };
  let mut guard = Guard {
    generation,
    kind: GuardKind::Starting(lane),
  };
  write_json(&mut write, &"ready").await?;
  let (mut read, mut write) = transport::server(read, write);
  let (op, state) = match saved {
    Some(saved) => (saved.opening, Some(saved.state)),
    None => {
      let op = timeout(IO_TIMEOUT, read_json(&mut read, CONTROL_LIMIT))
        .await??
        .ok_or_else(|| invalid("missing application opening"))?;
      (op, None)
    }
  };
  let gate = Gate::new(lane, state);
  {
    let mut registry = generation.registry.lock().unwrap();
    if registry.active.contains_key(&lane) {
      return Err(invalid("duplicate desktop lane"));
    }
    registry.starting.remove(&lane);
    registry.active.insert(lane, (op.clone(), gate.clone()));
    guard.kind = GuardKind::Active(lane, gate.clone());
  }
  let result = serve_opened(op, generation.rooms.clone(), &mut read, &mut write, &gate).await;
  if result.is_ok() && gate.phase() == Phase::Commit {
    timeout(IO_TIMEOUT, write.switch(read.consumed())).await??;
    read.confirm_switch().await?;
  }
  result
}
