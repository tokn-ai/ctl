use super::*;
use uuid::Uuid;
#[cfg(unix)]
mod restart;

fn request(name: &str) -> Join {
  Join {
    room: Room {
      target: ":1".into(),
      window: None,
    },
    participant: Participant {
      participant_id: Uuid::new_v4().to_string(),
      agent_id: name.into(),
    },
    subscribe: true,
    mode: Mode::Legacy,
  }
}

#[tokio::test]
async fn slow_subscriber_is_evicted_and_membership_cleanup_remains_visible() {
  let rooms = Rooms::default();
  let (slow_tx, mut slow_events) = mpsc::channel(1);
  let slow_evicted = Arc::new(Notify::new());
  let evict = slow_evicted.clone();
  let slow = Membership::register(rooms.clone(), request("slow"), slow_tx, evict).unwrap();
  // The unread initial snapshot occupies this participant's only queue slot.
  let (fast_tx, mut fast_events) = mpsc::channel(4);
  let evict = Arc::new(Notify::new());
  let fast = Membership::register(rooms.clone(), request("fast"), fast_tx, evict).unwrap();
  slow_evicted.notified().await;
  let id = slow.id.clone();
  drop(slow);
  assert!(matches!(
    &*slow_events.recv().await.unwrap(),
    LiveEvent::Snapshot { .. }
  ));
  assert!(matches!(
    &*fast_events.recv().await.unwrap(),
    LiveEvent::Snapshot { .. }
  ));
  assert_eq!(
    &*fast_events.recv().await.unwrap(),
    &LiveEvent::Left { participant_id: id }
  );
  drop(fast);
  assert!(rooms.state.lock().unwrap().rooms.is_empty());
}

#[tokio::test]
async fn malformed_action_disconnects_sender_without_forwarding_payload() {
  let rooms = Rooms::default();
  let (sender, server) = tokio::io::duplex(4096);
  let connection = tokio::spawn(connection(server, rooms.clone()));
  let mut sender = BufReader::new(sender);
  write_json(sender.get_mut(), &request("script"))
    .await
    .unwrap();
  assert_eq!(
    read_json::<bool>(&mut sender, 16).await.unwrap(),
    Some(true)
  );
  assert!(matches!(
    read_json::<LiveEvent>(&mut sender, 4096).await.unwrap(),
    Some(LiveEvent::Snapshot { .. })
  ));
  use tokio::io::AsyncWriteExt as _;
  sender
    .get_mut()
    .write_all(b"{\"operation\":\"type_text\",\"text\":\"hidden\"}\n")
    .await
    .unwrap();
  assert!(
    timeout(IO_TIMEOUT, connection)
      .await
      .unwrap()
      .unwrap()
      .is_err()
  );
  assert!(rooms.state.lock().unwrap().rooms.is_empty());
}

#[tokio::test]
async fn wire_limit_excludes_the_newline() {
  let mut wire = b"true".to_vec();
  wire.resize(EVENT_LIMIT, b' ');
  wire.push(b'\n');
  assert_eq!(
    read_json::<bool>(&mut wire.as_slice(), EVENT_LIMIT)
      .await
      .unwrap(),
    Some(true)
  );
  wire.insert(0, b' ');
  assert!(
    read_json::<bool>(&mut wire.as_slice(), EVENT_LIMIT)
      .await
      .is_err()
  );
}

pub(super) fn controller(rooms: &Rooms, target: &str, window: Option<&str>) -> Membership {
  let request = Join::new(
    ctream_proto::Target::parse(target).unwrap(),
    window,
    "same-label",
    &Uuid::new_v4().to_string(),
    false,
    Mode::Authority,
  )
  .unwrap();
  let (send, _) = mpsc::channel(8);
  let evict = Arc::new(Notify::new());
  Membership::register(rooms.clone(), request, send, evict).unwrap()
}

fn token(reply: Reply) -> String {
  if let Reply::Ok { value: Some(value) } = reply {
    value
  } else {
    panic!("expected an issued capability")
  }
}

fn conflict(reply: Reply, expected: &str, owner: Option<&str>) {
  let Reply::Conflict { code, owner_id } = reply else {
    panic!("expected {expected}")
  };
  assert_eq!(code, expected);
  assert_eq!(owner_id.as_deref(), owner);
}

fn observer(
  rooms: &Rooms,
  target: &str,
  window: Option<&str>,
  mode: Mode,
) -> (Membership, mpsc::Receiver<Arc<LiveEvent>>) {
  let request = Join::new(
    ctream_proto::Target::parse(target).unwrap(),
    window,
    "watcher",
    &Uuid::new_v4().to_string(),
    true,
    mode,
  )
  .unwrap();
  let (send, recv) = mpsc::channel(16);
  let evict = Arc::new(Notify::new());
  (
    Membership::register(rooms.clone(), request, send, evict).unwrap(),
    recv,
  )
}

#[tokio::test]
async fn a_disconnected_controller_leaves_pending_input_without_disturbing_live_controllers() {
  use crate::{activity::Store, desktop_input::InputRequest};

  let base = std::env::temp_dir().join(format!("ctream-admission-{}", Uuid::new_v4()));
  let rooms = Arc::new(Desktop {
    journal: Store::at(base.clone()),
    ..Desktop::default()
  });
  let owner = controller(&rooms, ":6789", None);
  let admitted = token(owner.command(Command::Begin { turn_id: None }));
  let (client, server) = tokio::io::duplex(4096);
  let service = tokio::spawn(connection(server, rooms.clone()));
  let mut reader = BufReader::new(client);
  let join = Join::new(
    ctream_proto::Target::X11(6789),
    None,
    "departed",
    &Uuid::new_v4().to_string(),
    false,
    Mode::Authority,
  )
  .unwrap();
  write_json(reader.get_mut(), &join).await.unwrap();
  assert!(matches!(
    read_json::<Reply>(&mut reader, CONTROL_LIMIT)
      .await
      .unwrap(),
    Some(Reply::Ok { .. })
  ));
  write_json(
    reader.get_mut(),
    &Command::Execute {
      turn_id: None,
      request: InputRequest::Raw {
        commands: vec!["button 1 1\n".into()],
      },
    },
  )
  .await
  .unwrap();
  // The first action still owns admission. No reply or driver is possible.
  assert!(
    timeout(
      Duration::from_millis(50),
      read_json::<Reply>(&mut reader, CONTROL_LIMIT)
    )
    .await
    .is_err()
  );
  drop(reader);
  timeout(Duration::from_secs(2), service)
    .await
    .expect("a disconnected waiter must wake before the holder finishes")
    .unwrap()
    .unwrap();

  let db = rusqlite::Connection::open(base.join("ctream/activity/activity.sqlite3")).unwrap();
  let row = db.query_row("SELECT a.status, r.refusal_code FROM actions a JOIN action_refusals r USING(action_id) WHERE a.session_id=?1 AND a.operation='pointer_down'", [&join.participant.participant_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).unwrap();
  assert_eq!(row, ("failed".into(), "stale_turn".into()));
  // Leaving a waiting controller cannot revoke the accepted action's worker.
  let mut pin = turns::Pin::attach(
    rooms.clone(),
    owner.room.clone(),
    owner.key.clone().unwrap(),
  )
  .unwrap();
  assert_eq!(token(pin.begin()), admitted);
  pin.finish();
  owner.command(Command::Finish {});
  let next = controller(&rooms, ":6789", None);
  token(next.command(Command::Begin { turn_id: None }));
  drop((next, owner, pin, db, rooms));
  std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn a_disconnected_admitted_controller_retains_its_delegation_until_cleanup() {
  let rooms = Rooms::default();
  let owner = controller(&rooms, ":7", None);
  let other = controller(&rooms, ":7", None);
  let key = owner.key.clone().unwrap();
  let admitted = token(owner.command(Command::Begin { turn_id: None }));
  rooms.stop_admitting(&key);
  let mut worker = turns::Pin::attach(rooms.clone(), owner.room.clone(), key).unwrap();
  assert_eq!(token(worker.begin()), admitted);
  assert!(matches!(
    owner.command(Command::Finish {}),
    Reply::Ok { .. }
  ));
  conflict(
    owner.command(Command::Begin { turn_id: None }),
    "stale_turn",
    None,
  );
  conflict(
    other.command(Command::Acquire { ttl_ms: 1000 }),
    "busy",
    Some(&owner.id),
  );
  worker.finish();
  token(other.command(Command::Acquire { ttl_ms: 1000 }));
}

#[test]
fn turns_bind_to_the_connection_fence_across_sibling_sources_and_expire_monotonically() {
  let rooms = Rooms::default();
  let owner = controller(&rooms, ":0", Some("x11-1"));
  let rival = controller(&rooms, "wayland-1", None);
  assert!(matches!(
    owner.command(Command::Acquire { ttl_ms: 0 }),
    Reply::Invalid { .. }
  ));
  assert!(matches!(
    owner.command(Command::Acquire { ttl_ms: 30_001 }),
    Reply::Invalid { .. }
  ));
  let turn = token(owner.command(Command::Acquire { ttl_ms: 10_000 }));
  conflict(
    rival.command(Command::Begin { turn_id: None }),
    "busy",
    Some(&owner.id),
  );
  conflict(
    rival.command(Command::Release {
      turn_id: turn.clone(),
    }),
    "stale_turn",
    None,
  );
  conflict(
    rival.command(Command::Begin {
      turn_id: Some(turn.clone()),
    }),
    "stale_turn",
    None,
  );
  conflict(
    owner.command(Command::Begin { turn_id: None }),
    "stale_turn",
    None,
  );
  let admission = token(owner.command(Command::Begin {
    turn_id: Some(turn.clone()),
  }));
  assert_ne!(turn, admission);
  rooms
    .state
    .lock()
    .unwrap()
    .expire(tokio::time::Instant::now() + Duration::from_secs(11));
  conflict(
    owner.command(Command::Release {
      turn_id: turn.clone(),
    }),
    "stale_turn",
    None,
  );
  conflict(
    rival.command(Command::Acquire { ttl_ms: 1_000 }),
    "busy",
    Some(&owner.id),
  );
  conflict(
    rival.command(Command::Begin { turn_id: None }),
    "busy",
    Some(&owner.id),
  );
  assert!(matches!(
    owner.command(Command::Finish {}),
    Reply::Ok { .. }
  ));
  conflict(
    owner.command(Command::Begin {
      turn_id: Some(turn),
    }),
    "stale_turn",
    None,
  );
  token(rival.command(Command::Acquire { ttl_ms: 1_000 }));
}

#[test]
fn delegates_pin_exact_source_and_only_the_live_parent_admission_until_driver_cleanup() {
  let rooms = Rooms::default();
  let owner = controller(&rooms, "windows-1", Some("win-A"));
  let rival = controller(&rooms, "windows-2", Some("win-B"));
  let key = owner.key.clone().unwrap();
  for wrong_key in [&owner.id, &Uuid::new_v4().to_string()] {
    assert!(turns::Pin::attach(rooms.clone(), owner.room.clone(), wrong_key.clone()).is_err());
  }
  for wrong_source in [
    Room::new(ctream_proto::Target::Windows(1), None),
    rival.room.clone(),
  ] {
    assert!(turns::Pin::attach(rooms.clone(), wrong_source, key.clone()).is_err());
  }
  let mut pin = turns::Pin::attach(rooms.clone(), owner.room.clone(), key.clone()).unwrap();
  let mut idle = turns::Pin::attach(rooms.clone(), owner.room.clone(), key.clone()).unwrap();
  conflict(pin.begin(), "stale_turn", None);
  let turn = token(owner.command(Command::Acquire { ttl_ms: 1_000 }));
  let admission = token(owner.command(Command::Begin {
    turn_id: Some(turn),
  }));
  assert_eq!(token(pin.begin()), admission);
  let id = owner.id.clone();
  let source = owner.room.clone();
  drop(owner);
  assert!(turns::Pin::attach(rooms.clone(), source, key).is_err());
  conflict(idle.begin(), "stale_turn", None);
  conflict(
    rival.command(Command::Acquire { ttl_ms: 1_000 }),
    "busy",
    Some(&id),
  );
  drop(pin);
  token(rival.command(Command::Begin { turn_id: None }));
}

#[test]
fn extended_watchers_track_desktop_leases_and_legacy_watchers_keep_prior_records() {
  let rooms = Rooms::default();
  let owner = controller(&rooms, "windows-1", Some("win-A"));
  let (_watch, mut ext) = observer(&rooms, "windows", Some("win-B"), Mode::Turns);
  let (_old, mut old) = observer(&rooms, "windows", Some("win-B"), Mode::Legacy);
  assert!(matches!(
    ext.try_recv().unwrap().as_ref(),
    LiveEvent::Snapshot { .. }
  ));
  assert_eq!(
    ext.try_recv().unwrap().as_ref(),
    &LiveEvent::Turn {
      holder: None,
      expires_at_ms: None
    }
  );
  assert!(matches!(
    ext.try_recv().unwrap().as_ref(),
    LiveEvent::Joined { .. }
  ));
  assert!(matches!(
    old.try_recv().unwrap().as_ref(),
    LiveEvent::Snapshot { .. }
  ));
  let turn = token(owner.command(Command::Acquire { ttl_ms: 10_000 }));
  assert!(
    matches!(ext.try_recv().unwrap().as_ref(),LiveEvent::Turn{holder:Some(p),expires_at_ms:Some(_)} if *p==owner.participant)
  );
  assert!(old.try_recv().is_err());
  let (_during, mut during) = observer(&rooms, "windows-1", Some("win-A"), Mode::Turns);
  assert!(matches!(
    during.try_recv().unwrap().as_ref(),
    LiveEvent::Snapshot { .. }
  ));
  assert!(
    matches!(during.try_recv().unwrap().as_ref(),LiveEvent::Turn{holder:Some(p),..} if *p==owner.participant)
  );
  assert!(matches!(
    owner.command(Command::Release { turn_id: turn }),
    Reply::Ok { .. }
  ));
  assert!(matches!(
    ext.try_recv().unwrap().as_ref(),
    LiveEvent::Turn { holder: None, .. }
  ));
  assert!(old.try_recv().is_err());
  assert!(matches!(
    during.try_recv().unwrap().as_ref(),
    LiveEvent::Turn { holder: None, .. }
  ));
  let (_legacy_same, mut legacy_same) = observer(&rooms, "windows-1", Some("win-A"), Mode::Legacy);
  assert!(matches!(
    legacy_same.try_recv().unwrap().as_ref(),
    LiveEvent::Snapshot { .. }
  ));
  assert!(matches!(
    during.try_recv().unwrap().as_ref(),
    LiveEvent::Joined { .. }
  ));
  let action_id = Uuid::new_v4().to_string();
  owner
    .publish_recorded(LiveAction::Invoke {}, Some(action_id.clone()))
    .unwrap();
  assert!(
    matches!(during.try_recv().unwrap().as_ref(),LiveEvent::Action{action_id:Some(id),..} if *id==action_id)
  );
  assert!(matches!(
    legacy_same.try_recv().unwrap().as_ref(),
    LiveEvent::Action {
      action_id: None,
      ..
    }
  ));
}

#[test]
fn only_competing_implicit_gestures_may_wait() {
  let rooms = Rooms::default();
  let a = controller(&rooms, ":2", None);
  let b = controller(&rooms, ":2", None);
  token(a.command(Command::Begin { turn_id: None }));
  assert!(
    matches!(b.command(Command::Begin{turn_id:None}),Reply::Pending{owner_id} if owner_id==a.id)
  );
  assert!(matches!(a.command(Command::Finish {}), Reply::Ok { .. }));
  token(a.command(Command::Acquire { ttl_ms: 10_000 }));
  conflict(
    b.command(Command::Begin { turn_id: None }),
    "busy",
    Some(&a.id),
  );
}

#[test]
fn confirmed_observers_receive_only_host_recorded_input() {
  let rooms = Rooms::default();
  let source = controller(&rooms, ":4", None);
  let (_subscriber, mut events) = observer(&rooms, ":4", None, Mode::Observed);
  assert!(matches!(
    *events.try_recv().unwrap(),
    LiveEvent::Snapshot { .. }
  ));
  assert!(matches!(
    *events.try_recv().unwrap(),
    LiveEvent::Turn { .. }
  ));
  source.publish(LiveAction::TypeText {}).unwrap();
  assert!(events.try_recv().is_err());
  let action_id = Uuid::new_v4().to_string();
  source
    .publish_recorded(LiveAction::TypeText {}, Some(action_id.clone()))
    .unwrap();
  assert!(
    matches!(&*events.try_recv().unwrap(), LiveEvent::Action { action_id:Some(id), .. } if id == &action_id)
  );
}
