use super::{
  CONTROL_LIMIT, EVENT_LIMIT, IO_TIMEOUT, Join, Mode, Opening, Room,
  authority::{Command, Reply},
  endpoint, invalid, read_json, write_json,
};
use ctream_proto::live::{LiveAction, LiveEvent, Participant};
use interprocess::local_socket::traits::tokio::Listener as _;
use std::{
  collections::BTreeMap,
  io,
  sync::{Arc, Mutex},
  time::Duration,
};
use tokio::{io::AsyncWrite, sync::Notify, task::JoinSet};

#[cfg(test)]
use tokio::{
  io::{AsyncRead, BufReader},
  time::timeout,
};

const MAX_CONNECTIONS: usize = 256;
const QUEUE_SIZE: usize = 64;
const IDLE_EXIT: Duration = Duration::from_secs(30);
pub(super) mod checkpoint;
mod controller;
pub(in crate::daemon) mod handoff;
pub(super) mod lane;
mod subscriptions;
use subscriptions::Events;
mod turns;

struct Member {
  mode: Mode,
  participant: Participant,
  events: Option<Events>,
  evict: Arc<Notify>,
}

type Members = BTreeMap<String, Member>;

struct ControllerRoute {
  source: Room,
  participant: String,
  accepting: bool,
}

#[derive(Default)]
struct State {
  rooms: BTreeMap<Room, Members>,
  turns: BTreeMap<turns::Domain, turns::Turns>,
  controllers: BTreeMap<String, ControllerRoute>,
  observers: BTreeMap<String, subscriptions::Binding>,
  lanes: BTreeMap<uuid::Uuid, checkpoint::Lane>,
  expiry_paused: bool,
  timer: Arc<tokio::sync::Notify>,
}
impl State {
  fn register_controller(&mut self, key: String, source: Room, participant: String) {
    self.controllers.insert(
      key,
      ControllerRoute {
        source,
        participant,
        accepting: true,
      },
    );
  }
}

#[derive(Default)]
pub(super) struct Desktop {
  journal: Arc<crate::activity::Store>,
  state: Mutex<State>,
  available: std::sync::Condvar,
  dormant: Mutex<BTreeMap<uuid::Uuid, checkpoint::Prepared>>,
}
pub(super) type Rooms = Arc<Desktop>;

fn broadcast(members: &mut Members, event: LiveEvent) {
  let event = Arc::new(event);
  let legacy = match &*event {
    LiveEvent::Turn { .. } => None,
    LiveEvent::Action {
      participant_id,
      action,
      action_id: Some(_),
    } => Some(Arc::new(LiveEvent::Action {
      participant_id: participant_id.clone(),
      action: *action,
      action_id: None,
    })),
    _ => Some(event.clone()),
  };
  for member in members.values_mut() {
    if member.mode == Mode::Observed
      && matches!(
        &*event,
        LiveEvent::Action {
          action_id: None,
          ..
        }
      )
    {
      continue;
    }
    let chosen = if matches!(member.mode, Mode::Turns | Mode::Observed) {
      Some(&event)
    } else {
      legacy.as_ref()
    };
    if let Some(events) = &member.events
      && let Some(chosen) = chosen
      && events.try_send(chosen.clone()).is_err()
    {
      // A slow subscriber must rejoin for a new snapshot. Never silently lose
      // a leave/join and keep delivering events against an incorrect roster.
      member.events = None;
      member.evict.notify_one();
    }
  }
}

struct Membership {
  rooms: Rooms,
  room: Room,
  id: String,
  participant: Participant,
  key: Option<String>,
  attachment: Option<subscriptions::Attached>,
  registration: Option<checkpoint::Registration>,
}

impl Membership {
  #[cfg(test)]
  fn register(
    rooms: Rooms,
    request: Join,
    events: impl Into<Events>,
    evict: Arc<Notify>,
  ) -> io::Result<Self> {
    let key = (request.mode == Mode::Authority).then(|| uuid::Uuid::new_v4().to_string());
    Self::open(rooms, request, events.into(), evict, key, false)
  }

  fn open(
    rooms: Rooms,
    request: Join,
    events: Events,
    evict: Arc<Notify>,
    key: Option<String>,
    restoring: bool,
  ) -> io::Result<Self> {
    request.validate()?;
    let room = request.room;
    let id = request.participant.participant_id.clone();
    let mut state = rooms.state.lock().unwrap();
    state.expire(tokio::time::Instant::now());
    let turn_event =
      matches!(request.mode, Mode::Turns | Mode::Observed).then(|| state.turn_event(&room));
    let members = state.rooms.entry(room.clone()).or_default();
    if members.contains_key(&id) {
      return Err(invalid("live session is already registered"));
    }
    if request.subscribe && !restoring {
      let participants = members
        .values()
        .map(|m| m.participant.clone())
        .chain(std::iter::once(request.participant.clone()))
        .collect();
      events
        .try_send(Arc::new(LiveEvent::Snapshot {
          self_id: id.clone(),
          participants,
        }))
        .map_err(|_| io::Error::other("could not deliver initial live roster"))?;
      if let Some(turn) = turn_event {
        events
          .try_send(Arc::new(turn))
          .map_err(|_| invalid("could not deliver initial turn"))?;
      }
    }
    if !restoring {
      broadcast(
        members,
        LiveEvent::Joined {
          participant: request.participant.clone(),
        },
      );
    }
    members.insert(
      id.clone(),
      Member {
        participant: request.participant.clone(),
        mode: request.mode,
        events: request.subscribe.then_some(events),
        evict,
      },
    );
    if let Some(key) = &key {
      state.register_controller(key.clone(), room.clone(), id.clone());
    }
    drop(state);
    Ok(Self {
      rooms,
      room,
      id,
      participant: request.participant,
      key,
      attachment: None,
      registration: None,
    })
  }

  fn publish(&self, action: LiveAction) -> io::Result<()> {
    self.publish_recorded(action, None)
  }
  fn publish_recorded(&self, action: LiveAction, action_id: Option<String>) -> io::Result<()> {
    if !action.valid() {
      return Err(invalid("invalid normalized live action"));
    }
    let mut state = self.rooms.state.lock().unwrap();
    let members = state.rooms.get_mut(&self.room).expect("registered room");
    broadcast(
      members,
      LiveEvent::Action {
        participant_id: self.id.clone(),
        action,
        action_id,
      },
    );
    Ok(())
  }
}

impl Drop for Membership {
  fn drop(&mut self) {
    let mut state = self.rooms.state.lock().unwrap();
    if let Some(key) = &self.key {
      state.controllers.remove(key);
      state.close_authority(&self.room, key);
      self.rooms.available.notify_all();
    }
    if let Some(attached) = &self.attachment {
      if let Some(binding) = state.observers.get_mut(&attached.capability) {
        binding.revoked = None;
      }
      return;
    }
    let members = state.rooms.get_mut(&self.room).expect("registered room");
    members.remove(&self.id);
    if members.is_empty() {
      state.rooms.remove(&self.room);
    } else {
      broadcast(
        members,
        LiveEvent::Left {
          participant_id: self.id.clone(),
        },
      );
    }
  }
}

#[cfg(test)]
async fn connection(stream: impl AsyncRead + AsyncWrite + Unpin, rooms: Rooms) -> io::Result<()> {
  let (read, mut write) = tokio::io::split(stream);
  let mut reader = BufReader::new(read);
  let Some(opening) =
    timeout(IO_TIMEOUT, read_json::<Opening>(&mut reader, CONTROL_LIMIT)).await??
  else {
    return Ok(());
  };
  serve_opened(
    opening,
    rooms,
    &mut reader,
    &mut write,
    &lane::Gate::legacy(),
  )
  .await
}

pub(super) async fn serve_opened(
  opening: Opening,
  rooms: Rooms,
  reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
  write: &mut (impl AsyncWrite + Unpin),
  gate: &lane::Gate,
) -> io::Result<()> {
  use lane::Phase;
  let request = match opening {
    Opening::Lane { .. } | Opening::Maintenance { .. } => {
      return Err(invalid("a lane requires an application operation"));
    }
    #[cfg(any(target_os = "linux", windows))]
    Opening::Capture { source } => return super::capture::serve(source, reader, write, gate).await,
    Opening::ObserveSources {
      observe_sources,
      agent_id,
    } => {
      return subscriptions::serve_lane(rooms, observe_sources, agent_id, reader, write, gate)
        .await;
    }
    Opening::AttachControl { attachment, source } => {
      let attached = subscriptions::attach_lane(rooms, &attachment, &source, gate.id());
      return match attached {
        Ok(member) => controller::serve(member, reader, write, gate).await,
        Err(error) => write_json(write, &Reply::invalid(&error.to_string())).await,
      };
    }
    Opening::Join(join) => join,
    Opening::Delegate {
      room,
      delegation_key,
    } => {
      let mut pin = turns::Pin::attach(rooms, room, delegation_key)?;
      let mut active = false;
      if !gate.resumed() {
        write_json(write, &Reply::done()).await?;
      }
      loop {
        // Controllers may still be dispatching through a delegated driver
        // during DrainInput. A pin must remain responsive until they drain.
        let command = tokio::select! {
          biased;
          _ = gate.next_output_barrier(), if !active => {
            if gate.park(Phase::Freeze, Phase::Freeze, None).await { return Ok(()); }
            continue;
          }
          command = read_json::<Command>(reader, CONTROL_LIMIT) => command?,
        };
        let Some(command) = command else {
          return Ok(());
        };
        let result = match command {
          Command::Begin { turn_id: None } => {
            let result = pin.begin();
            if matches!(result, Reply::Ok { .. }) {
              active = true;
            }
            result
          }
          Command::Finish {} => {
            let result = pin.finish();
            if matches!(result, Reply::Ok { .. }) {
              active = false;
            }
            result
          }
          _ => Reply::invalid("invalid delegated driver command"),
        };
        write_json(write, &result).await?;
      }
    }
  };
  let subscribe = request.subscribe;
  let confirmed_only = request.mode == Mode::Observed;
  let (participant, mut events, evicted) = checkpoint::join(rooms, request, gate.id())?;
  if participant.key.is_some() {
    return controller::serve(participant, reader, write, gate).await;
  }
  if !gate.resumed() {
    write_json(write, &true).await?;
  }
  if !subscribe {
    loop {
      let action = tokio::select! {
        biased;
        _ = gate.requested(Phase::DrainInput) => {
          if gate.park(Phase::DrainInput, Phase::Freeze, None).await { return Ok(()); }
          continue;
        }
        action = read_json::<LiveAction>(reader, EVENT_LIMIT) => action?,
      };
      let Some(action) = action else {
        return Ok(());
      };
      participant.publish(action)?;
      write_json(write, &true).await?;
    }
  }
  let read_actions = async {
    loop {
      let action = tokio::select! {
        biased;
        _ = gate.requested(Phase::DrainInput) => {
          if gate.park(Phase::DrainInput, Phase::DrainInput, None).await { return Ok(()); }
          continue;
        }
        action = read_json::<LiveAction>(reader, EVENT_LIMIT) => action?,
      };
      let Some(action) = action else {
        return Ok(());
      };
      if confirmed_only {
        return Err(invalid("daemon observation is read-only"));
      }
      participant.publish(action)?;
    }
  };
  let send_events = async {
    loop {
      tokio::select! {
        biased;
        _ = gate.requested(Phase::Freeze) => {
          while let Ok(event) = events.try_recv() {
            write_json(write, event.as_ref()).await?;
          }
          if gate.park(Phase::Freeze, Phase::Freeze, None).await {return Ok(());}
        }
        next = events.recv() => {
          let Some(event) = next else {return Ok(());};
          write_json(write, &*event).await?;
        }
      }
    }
  };
  tokio::select! {
    result = read_actions => result,
    result = send_events => result,
    _ = evicted.notified() => Err(io::Error::other("live subscriber fell behind")),
  }
}

pub(super) async fn serve() -> io::Result<()> {
  let start = endpoint::lock("upgrade.lock")?;
  start.try_lock_shared()?;
  let endpoint = endpoint::bind()?;
  endpoint::wait_for_old_gestures()?;
  let state = super::generation::recover()?.unwrap_or_default();
  let _ = super::generation::discard_unused_images();
  drop(start);
  serve_generation(endpoint, state).await
}

pub(in crate::daemon) async fn serve_generation(
  endpoint: endpoint::Endpoint,
  generation: handoff::Generation,
) -> io::Result<()> {
  let generation = Arc::new(generation);
  generation.start_expiry();
  tokio::select! {
    result=accept(endpoint,generation.clone())=>result,
    _=turns::expire_leases(generation.rooms.clone())=>unreachable!("expiry scheduler only stops at shutdown")
  }
}

async fn accept(
  endpoint: endpoint::Endpoint,
  generation: Arc<handoff::Generation>,
) -> io::Result<()> {
  let mut clients = JoinSet::new();
  loop {
    let accepted = if clients.is_empty() {
      match tokio::time::timeout_at(generation.idle_deadline(), endpoint.listener.accept()).await {
        Ok(result) => result,
        Err(_) => return Ok(()),
      }
    } else {
      tokio::select! {
        biased;
        () = generation.commit_signal() => {
          // Keep the election lock until every accepted session has received
          // its last reply and SWITCH record on the predecessor's socket.
          drop(endpoint.listener);
          while clients.join_next().await.is_some() {}
          return Ok(());
        }
        _ = clients.join_next() => continue,
        result = endpoint.listener.accept() => result,
      }
    };
    let stream = match accepted {
      Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
      other => other?,
    };
    // A full desktop must still admit the updater and a passive status query.
    // The route enforces the application limit after reading its opening.
    if clients.len() < MAX_CONNECTIONS + 2 && endpoint::check_client(&stream).is_ok() {
      let generation = generation.clone();
      clients.spawn(async move {
        let result = handoff::route(&stream, &generation).await;
        endpoint::discard_pending(&stream);
        result
      });
    }
  }
}

#[cfg(test)]
mod tests;
