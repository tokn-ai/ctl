use super::{Join, Membership, Mode, Participant, QUEUE_SIZE, Room, Rooms};
use crate::daemon::{invalid, observer, write_json};
use ctream_proto::live::LiveEvent;
use std::{
  io,
  sync::{Arc, Weak},
};
use tokio::{
  io::{AsyncRead, AsyncReadExt, AsyncWrite},
  sync::{Notify, mpsc},
};

type Tagged = (usize, Arc<LiveEvent>);

pub(super) enum Events {
  Single(mpsc::Sender<Arc<LiveEvent>>),
  Multiple {
    source: usize,
    output: mpsc::Sender<Tagged>,
  },
}

impl From<mpsc::Sender<Arc<LiveEvent>>> for Events {
  fn from(output: mpsc::Sender<Arc<LiveEvent>>) -> Self {
    Self::Single(output)
  }
}

impl Events {
  pub(super) fn try_send(&self, event: Arc<LiveEvent>) -> Result<(), ()> {
    match self {
      Self::Single(output) => output.try_send(event).map_err(|_| ()),
      Self::Multiple { source, output } => output.try_send((*source, event)).map_err(|_| ()),
    }
  }
}

/// The event connection owns the secret; attached lanes retain membership
/// until their already-dispatched input and platform cleanup have finished.
pub(super) struct Binding {
  sources: Weak<Vec<Membership>>,
  pub(super) revoked: Option<Arc<Notify>>,
}

pub(super) struct Attached {
  pub(super) capability: String,
  pub(super) revoked: Arc<Notify>,
  _sources: Arc<Vec<Membership>>,
}

struct Owner {
  rooms: Rooms,
  capability: String,
  _sources: Arc<Vec<Membership>>,
}

impl Owner {
  fn register(rooms: Rooms, subscriptions: Vec<Membership>) -> Self {
    let sources = Arc::new(subscriptions);
    let capability = uuid::Uuid::new_v4().to_string();
    rooms.state.lock().unwrap().observers.insert(
      capability.clone(),
      Binding {
        sources: Arc::downgrade(&sources),
        revoked: None,
      },
    );
    Self {
      rooms,
      capability,
      _sources: sources,
    }
  }
}

impl Drop for Owner {
  fn drop(&mut self) {
    let mut state = self.rooms.state.lock().unwrap();
    if let Some(binding) = state.observers.remove(&self.capability)
      && let Some(revoked) = binding.revoked
    {
      revoked.notify_one();
    }
    self.rooms.available.notify_all();
  }
}

pub(super) fn attach(rooms: Rooms, capability: &str, source: &Room) -> io::Result<Membership> {
  let mut state = rooms.state.lock().unwrap();
  let binding = state
    .observers
    .get_mut(capability)
    .ok_or_else(|| invalid("invalid observer attachment"))?;
  if binding.revoked.is_some() {
    return Err(invalid("observer already has a control lane"));
  }
  let sources = binding
    .sources
    .upgrade()
    .ok_or_else(|| invalid("observer disconnected"))?;
  let observed = sources
    .iter()
    .find(|observed| &observed.room == source)
    .ok_or_else(|| invalid("unknown observed source"))?;
  let participant = observed.participant.clone();
  let room = observed.room.clone();
  let key = uuid::Uuid::new_v4().to_string();
  let revoked = Arc::new(Notify::new());
  binding.revoked = Some(revoked.clone());
  state.register_controller(key.clone(), room.clone());
  drop(state);
  Ok(Membership {
    rooms,
    room,
    id: participant.participant_id.clone(),
    participant,
    key: Some(key),
    attachment: Some(Attached {
      capability: capability.to_owned(),
      revoked,
      _sources: sources,
    }),
  })
}

pub(super) async fn serve(
  rooms: Rooms,
  sources: Vec<Room>,
  agent_id: String,
  mut read: impl AsyncRead + Unpin,
  mut write: impl AsyncWrite + Unpin,
) -> io::Result<()> {
  observer::validate(&sources, &agent_id)?;
  let participant = Participant {
    participant_id: uuid::Uuid::new_v4().to_string(),
    agent_id,
  };
  let (output, mut events) = mpsc::channel(QUEUE_SIZE);
  let evicted = Arc::new(Notify::new());
  let mut subscriptions = Vec::with_capacity(sources.len());
  for (source, room) in sources.into_iter().enumerate() {
    subscriptions.push(Membership::register(
      rooms.clone(),
      Join {
        room,
        participant: participant.clone(),
        subscribe: true,
        mode: Mode::Observed,
      },
      Events::Multiple {
        source,
        output: output.clone(),
      },
      evicted.clone(),
    )?);
  }
  drop(output);
  let owner = Owner::register(rooms, subscriptions);
  let send = async {
    write_json(&mut write, &owner.capability).await?;
    while let Some((source, event)) = events.recv().await {
      write_json(&mut write, &(source, event.as_ref())).await?;
    }
    Ok(())
  };
  let mut byte = [0u8];
  // Readers can leave even while the single bounded output queue is backpressured.
  let result = tokio::select! {
    count = read.read(&mut byte) => match count? {
      0 => Ok(()),
      _ => Err(invalid("source observation is read-only")),
    },
    result = send => result,
    _ = evicted.notified() => Err(invalid("observer fell behind; reconnect for snapshots")),
  };
  drop(owner);
  result
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::daemon::{EVENT_LIMIT, read_json};
  use ctream_proto::{Target, live::LiveAction};
  use tokio::{
    io::BufReader,
    time::{Duration, timeout},
  };

  fn source(name: &str) -> Room {
    Room::new(Target::X11(4), Some(name))
  }

  #[tokio::test]
  async fn stalled_observer_is_removed_from_all_sources_without_delaying_others() {
    let rooms = Rooms::default();
    let sources = vec![source("x11-a"), source("x11-b")];
    let (socket, client) = tokio::io::duplex(128);
    let (read, write) = tokio::io::split(socket);
    let service = tokio::spawn(serve(
      rooms.clone(),
      sources.clone(),
      "slow".into(),
      read,
      write,
    ));
    let mut stalled = BufReader::new(client);
    assert!(
      uuid::Uuid::try_parse(
        &read_json::<String>(&mut stalled, EVENT_LIMIT)
          .await
          .unwrap()
          .unwrap()
      )
      .is_ok()
    );

    let actor = Join::new(
      Target::X11(4),
      Some("x11-b"),
      "actor",
      &uuid::Uuid::new_v4().to_string(),
      false,
      Mode::Authority,
    )
    .unwrap();
    let (events, _) = mpsc::channel(1);
    let actor = Membership::register(rooms.clone(), actor, events, Arc::default()).unwrap();
    let fast = Join::new(
      Target::X11(4),
      Some("x11-b"),
      "fast",
      &uuid::Uuid::new_v4().to_string(),
      true,
      Mode::Observed,
    )
    .unwrap();
    let (events, mut fast_events) = mpsc::channel(4);
    let _fast = Membership::register(rooms.clone(), fast, events, Arc::default()).unwrap();
    assert!(matches!(
      *fast_events.recv().await.unwrap(),
      LiveEvent::Snapshot { .. }
    ));
    assert!(matches!(
      *fast_events.recv().await.unwrap(),
      LiveEvent::Turn { .. }
    ));
    let id = uuid::Uuid::new_v4().to_string();
    for _ in 0..=QUEUE_SIZE {
      actor
        .publish_recorded(LiveAction::PressKeys {}, Some(id.clone()))
        .unwrap();
      assert!(matches!(
        *fast_events.recv().await.unwrap(),
        LiveEvent::Action { .. }
      ));
    }
    let result = timeout(Duration::from_secs(2), service)
      .await
      .unwrap()
      .unwrap();
    assert!(result.unwrap_err().to_string().contains("fell behind"));
    let state = rooms.state.lock().unwrap();
    assert!(!state.rooms.contains_key(&sources[0]));
    let members = state.rooms.get(&sources[1]).unwrap();
    assert_eq!(members.len(), 2);
    assert!(members.values().all(|m| m.participant.agent_id != "slow"));
  }

  #[test]
  fn control_requires_a_live_private_attachment_and_revocation_fences_queued_input() {
    use super::super::tests::controller;
    use crate::daemon::authority::{Command, Reply};
    let rooms = Rooms::default();
    let session = uuid::Uuid::new_v4().to_string();
    let request = Join::new(
      Target::X11(4),
      Some("x11-a"),
      "computer",
      &session,
      true,
      Mode::Observed,
    )
    .unwrap();
    let (events, _received) = mpsc::channel(8);
    let membership = Membership::register(rooms.clone(), request, events, Arc::default()).unwrap();
    let owner = Owner::register(rooms.clone(), vec![membership]);
    let capability = owner.capability.clone();
    assert!(
      attach(rooms.clone(), &session, &source("x11-a")).is_err(),
      "a public ID cannot attach"
    );
    assert!(
      attach(rooms.clone(), &capability, &source("x11-b")).is_err(),
      "unobserved source"
    );
    let lane = attach(rooms.clone(), &capability, &source("x11-a")).unwrap();
    assert_eq!(lane.id, session);
    assert!(
      attach(rooms.clone(), &capability, &source("x11-a")).is_err(),
      "only one live control lane"
    );
    let holder = controller(&rooms, ":4", Some("x11-b"));
    assert!(matches!(
      holder.command(Command::Begin { turn_id: None }),
      Reply::Ok { .. }
    ));
    assert!(matches!(
      lane.command(Command::Begin { turn_id: None }),
      Reply::Pending { .. }
    ));
    std::thread::scope(|scope| {
      let wait = scope.spawn(|| lane.admit(None));
      drop(owner);
      assert!(
        attach(rooms.clone(), &capability, &source("x11-a")).is_err(),
        "revoked attachment"
      );
      assert!(matches!(wait.join().unwrap(), Reply::Conflict{code, ..} if code == "stale_turn"));
    });
    assert!(matches!(
      holder.command(Command::Finish {}),
      Reply::Ok { .. }
    ));
    assert!(
      matches!(lane.command(Command::Acquire{ttl_ms:1000}), Reply::Conflict{code, ..} if code == "stale_turn")
    );
    drop(lane);
    assert!(
      !rooms
        .state
        .lock()
        .unwrap()
        .rooms
        .contains_key(&source("x11-a"))
    );
  }

  #[test]
  fn reject_duplicates_empty_sets_and_cross_desktop_subscriptions() {
    let one = source("x11-a");
    let mac = Room::new(Target::MacosDesktop, Some("mac-1"));
    for sources in [
      vec![],
      vec![one.clone(), one.clone()],
      vec![one.clone(), mac],
      vec![one; 17],
    ] {
      assert!(observer::validate(&sources, "observer").is_err());
    }
  }
}
