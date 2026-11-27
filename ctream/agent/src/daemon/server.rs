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
use tokio::{
  io::{AsyncRead, AsyncWrite, BufReader},
  sync::{Notify, mpsc},
  task::JoinSet,
  time::timeout,
};

const MAX_CONNECTIONS: usize = 256;
const QUEUE_SIZE: usize = 64;
const IDLE_EXIT: Duration = Duration::from_secs(30);
mod controller;
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
  accepting: bool,
}

#[derive(Default)]
struct State {
  rooms: BTreeMap<Room, Members>,
  turns: BTreeMap<turns::Domain, turns::Turns>,
  controllers: BTreeMap<String, ControllerRoute>,
  observers: BTreeMap<String, subscriptions::Binding>,
  timer: Arc<tokio::sync::Notify>,
}
impl State {
  fn register_controller(&mut self, key: String, source: Room) {
    self.controllers.insert(
      key,
      ControllerRoute {
        source,
        accepting: true,
      },
    );
  }
}

#[derive(Default)]
struct Desktop {
  journal: Arc<crate::activity::Store>,
  state: Mutex<State>,
  available: std::sync::Condvar,
}
type Rooms = Arc<Desktop>;

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
}

impl Membership {
  fn register(
    rooms: Rooms,
    request: Join,
    events: impl Into<Events>,
    evict: Arc<Notify>,
  ) -> io::Result<Self> {
    request.validate()?;
    let events = events.into();
    let room = request.room;
    let id = request.participant.participant_id.clone();
    let mut state = rooms.state.lock().unwrap();
    let key = (request.mode == Mode::Authority).then(|| uuid::Uuid::new_v4().to_string());
    state.expire(tokio::time::Instant::now());
    let turn_event =
      matches!(request.mode, Mode::Turns | Mode::Observed).then(|| state.turn_event(&room));
    let members = state.rooms.entry(room.clone()).or_default();
    if members.contains_key(&id) {
      return Err(invalid("live session is already registered"));
    }
    if request.subscribe {
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
    broadcast(
      members,
      LiveEvent::Joined {
        participant: request.participant.clone(),
      },
    );
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
      state.register_controller(key.clone(), room.clone());
    }
    drop(state);
    Ok(Self {
      rooms,
      room,
      id,
      participant: request.participant,
      key,
      attachment: None,
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

async fn connection(stream: impl AsyncRead + AsyncWrite + Unpin, rooms: Rooms) -> io::Result<()> {
  let (read, mut write) = tokio::io::split(stream);
  let mut reader = BufReader::new(read);
  let Some(opening) =
    timeout(IO_TIMEOUT, read_json::<Opening>(&mut reader, CONTROL_LIMIT)).await??
  else {
    return Ok(());
  };
  let request = match opening {
    #[cfg(any(target_os = "linux", windows))]
    Opening::Capture { source } => return super::capture::serve(source, reader, write).await,
    Opening::ObserveSources {
      observe_sources,
      agent_id,
    } => {
      return subscriptions::serve(rooms, observe_sources, agent_id, reader, write).await;
    }
    Opening::AttachControl { attachment, source } => {
      let attached = subscriptions::attach(rooms, &attachment, &source);
      return match attached {
        Ok(member) => controller::serve(member, &mut reader, &mut write).await,
        Err(error) => write_json(&mut write, &Reply::invalid(&error.to_string())).await,
      };
    }
    Opening::Join(join) => join,
    Opening::Delegate {
      room,
      delegation_key,
    } => {
      let mut pin = turns::Pin::attach(rooms, room, delegation_key)?;
      write_json(&mut write, &Reply::done()).await?;
      while let Some(command) = read_json::<Command>(&mut reader, CONTROL_LIMIT).await? {
        let result = match command {
          Command::Begin { turn_id: None } => pin.begin(),
          Command::Finish {} => pin.finish(),
          _ => Reply::invalid("invalid delegated driver command"),
        };
        write_json(&mut write, &result).await?;
      }
      return Ok(());
    }
  };
  let subscribe = request.subscribe;
  let confirmed_only = request.mode == Mode::Observed;
  let (sender, mut events) = mpsc::channel(QUEUE_SIZE);
  let evicted = Arc::new(Notify::new());
  let participant = Membership::register(rooms, request, sender, evicted.clone())?;
  if participant.key.is_some() {
    return controller::serve(participant, &mut reader, &mut write).await;
  }
  write_json(&mut write, &true).await?;
  if !subscribe {
    while let Some(action) = read_json::<LiveAction>(&mut reader, EVENT_LIMIT).await? {
      participant.publish(action)?;
      write_json(&mut write, &true).await?;
    }
    return Ok(());
  }
  let read_actions = async {
    while let Some(action) = read_json::<LiveAction>(&mut reader, EVENT_LIMIT).await? {
      if confirmed_only {
        return Err(invalid("daemon observation is read-only"));
      }
      participant.publish(action)?;
    }
    Ok(())
  };
  let send_events = async {
    while let Some(event) = events.recv().await {
      write_json(&mut write, &*event).await?;
    }
    Ok(())
  };
  tokio::select! {
    result = read_actions => result,
    result = send_events => result,
    _ = evicted.notified() => Err(io::Error::other("live subscriber fell behind")),
  }
}

pub(super) async fn serve() -> io::Result<()> {
  let endpoint = endpoint::bind()?;
  endpoint::wait_for_old_gestures()?;
  let rooms = Rooms::default();
  tokio::select! {
    result=accept(endpoint,rooms.clone())=>result,
    _=turns::expire_leases(rooms)=>unreachable!("expiry scheduler only stops at shutdown")
  }
}

async fn accept(endpoint: endpoint::Endpoint, rooms: Rooms) -> io::Result<()> {
  let mut clients = JoinSet::new();
  loop {
    let accepted = if clients.is_empty() {
      match timeout(IDLE_EXIT, endpoint.listener.accept()).await {
        Ok(result) => result,
        Err(_) => return Ok(()),
      }
    } else {
      tokio::select! {
        result = endpoint.listener.accept() => result,
        _ = clients.join_next() => continue,
      }
    };
    let stream = match accepted {
      Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
      other => other?,
    };
    if clients.len() < MAX_CONNECTIONS && endpoint::check_client(&stream).is_ok() {
      let rooms = rooms.clone();
      clients.spawn(async move {
        let result = connection(&stream, rooms).await;
        endpoint::discard_pending(&stream);
        result
      });
    }
  }
}

#[cfg(test)]
mod tests;
