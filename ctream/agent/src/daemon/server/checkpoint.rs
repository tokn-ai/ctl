//! Logical participants survive a planned daemon handoff. A restored lane owns
//! a normal membership and event queue before any new client can observe it.
use super::{
  Desktop, Join, Membership, Mode, Participant, QUEUE_SIZE, Room, Rooms, invalid,
  subscriptions::{self, Events},
  turns::SavedLease,
};
use crate::daemon::Opening;
use ctream_proto::live::LiveEvent;
use serde::{Deserialize, Serialize};
use std::{
  collections::{BTreeMap, BTreeSet},
  io,
  sync::{Arc, Weak},
};
use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Lane {
  Join {
    request: Join,
    key: Option<String>,
  },
  Observer {
    sources: Vec<Room>,
    participant: Participant,
    capability: String,
  },
  Attached {
    capability: String,
    source: Room,
    key: String,
  },
}

// The transport owns the lane secret. This registration ties its logical
// checkpoint record to the underlying membership's ordinary EOF cleanup.
pub(super) struct Registration {
  rooms: Weak<Desktop>,
  id: Uuid,
}

impl Registration {
  pub(super) fn install(
    rooms: &Rooms,
    lane: Option<Uuid>,
    record: impl FnOnce() -> Lane,
  ) -> io::Result<Option<Self>> {
    let Some(id) = lane else { return Ok(None) };
    let mut state = rooms.state.lock().unwrap();
    let std::collections::btree_map::Entry::Vacant(slot) = state.lanes.entry(id) else {
      return Err(invalid("duplicate handoff lane"));
    };
    slot.insert(record());
    Ok(Some(Self {
      rooms: Arc::downgrade(rooms),
      id,
    }))
  }
}

impl Drop for Registration {
  fn drop(&mut self) {
    if let Some(rooms) = self.rooms.upgrade() {
      rooms.state.lock().unwrap().lanes.remove(&self.id);
    }
  }
}

type Joined = (Membership, mpsc::Receiver<Arc<LiveEvent>>, Arc<Notify>);

pub(super) enum Prepared {
  Join(Joined),
  Observer(subscriptions::Observer),
  Attached(Membership),
}

pub(super) fn claim(
  rooms: &Rooms,
  lane: Option<Uuid>,
  matches: impl FnOnce(&Lane) -> bool,
) -> io::Result<Option<Prepared>> {
  let Some(id) = lane else { return Ok(None) };
  let mut dormant = rooms.dormant.lock().unwrap();
  let state = rooms.state.lock().unwrap();
  if let Some(saved) = state.lanes.get(&id)
    && (!matches(saved) || !dormant.contains_key(&id))
  {
    return Err(invalid("handoff lane already active or opening differs"));
  }
  Ok(dormant.remove(&id))
}

pub(super) fn join(rooms: Rooms, request: Join, lane: Option<Uuid>) -> io::Result<Joined> {
  if let Some(ready) = claim(
    &rooms,
    lane,
    |saved| matches!(saved, Lane::Join { request: old, .. } if old == &request),
  )? {
    let Prepared::Join(subscription) = ready else {
      unreachable!()
    };
    return Ok(subscription);
  }
  let key = (request.mode == Mode::Authority).then(|| Uuid::new_v4().to_string());
  prepare_join(rooms, request, key, lane, false)
}

fn prepare_join(
  rooms: Rooms,
  request: Join,
  key: Option<String>,
  lane: Option<Uuid>,
  restoring: bool,
) -> io::Result<Joined> {
  let (send, events) = mpsc::channel(QUEUE_SIZE);
  let evicted = Arc::new(Notify::new());
  let mut member = Membership::open(
    rooms.clone(),
    request.clone(),
    Events::Single(send),
    evicted.clone(),
    key.clone(),
    restoring,
  )?;
  member.registration = Registration::install(&rooms, lane, || Lane::Join { request, key })?;
  Ok((member, events, evicted))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::daemon) struct Checkpoint {
  lanes: BTreeMap<Uuid, Lane>,
  leases: Vec<SavedLease>,
}

impl Checkpoint {
  /// The record transport and desktop state snapshot independently at their
  /// barriers. Only claimable application lanes may enter the successor.
  pub(in crate::daemon) fn validate_openings<'a>(
    &self,
    openings: impl Iterator<Item = (Uuid, &'a Opening)>,
  ) -> io::Result<()> {
    let mut matched = 0;
    for (id, opening) in openings {
      let matches = match (self.lanes.get(&id), opening) {
        (Some(Lane::Join { request, .. }), Opening::Join(original)) => request == original,
        (
          Some(Lane::Observer {
            sources,
            participant,
            ..
          }),
          Opening::ObserveSources {
            observe_sources,
            agent_id,
          },
        ) => sources == observe_sources && &participant.agent_id == agent_id,
        (
          Some(Lane::Attached {
            capability, source, ..
          }),
          Opening::AttachControl {
            attachment,
            source: original,
          },
        ) => capability == attachment && source == original,
        #[cfg(any(target_os = "linux", windows))]
        (None, Opening::Capture { source }) => {
          source.validate()?;
          continue;
        }
        _ => false,
      };
      if !matches {
        return Err(invalid("handoff transport and desktop sessions differ"));
      }
      matched += 1;
    }
    if matched != self.lanes.len() {
      return Err(invalid("missing handoff transport lane"));
    }
    Ok(())
  }

  /// The transport must have parked admissions, drained its output streams,
  /// and kept each lane alive before reaching this cut.
  pub(in crate::daemon) fn capture(rooms: &Rooms) -> io::Result<Self> {
    if !rooms.dormant.lock().unwrap().is_empty() {
      return Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "previous handoff still has suspended sessions",
      ));
    }
    let state = rooms.state.lock().unwrap();
    let mut members = 0;
    let mut controllers = 0;
    let mut observers = 0;
    for lane in state.lanes.values() {
      match lane {
        Lane::Join { key, .. } => {
          members += 1;
          controllers += usize::from(key.is_some());
        }
        Lane::Observer { sources, .. } => {
          members += sources.len();
          observers += 1;
        }
        Lane::Attached { .. } => controllers += 1,
      }
    }
    if state.rooms.values().map(BTreeMap::len).sum::<usize>() != members
      || state.observers.len() != observers
      || state.controllers.len() != controllers
      || state.controllers.values().any(|route| !route.accepting)
    {
      return Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "desktop has a legacy or closing session",
      ));
    }
    Ok(Self {
      lanes: state.lanes.clone(),
      leases: state.save_leases()?,
    })
  }

  /// Install the full roster into an unpublished, empty desktop. Each dormant
  /// observer immediately captures peer changes into its normal bounded FIFO.
  /// Old initial handshakes and synthetic joins are never queued.
  pub(in crate::daemon) fn restore(&self, rooms: &Rooms) -> io::Result<()> {
    {
      let state = rooms.state.lock().unwrap();
      if !state.rooms.is_empty()
        || !state.controllers.is_empty()
        || !state.observers.is_empty()
        || !state.lanes.is_empty()
      {
        return Err(invalid("handoff requires a fresh desktop"));
      }
    }
    self.validate()?;
    let mut prepared = BTreeMap::new();
    for (&id, lane) in &self.lanes {
      let subscription = match lane {
        Lane::Join { request, key } => Prepared::Join(prepare_join(
          rooms.clone(),
          request.clone(),
          key.clone(),
          Some(id),
          true,
        )?),
        Lane::Observer {
          sources,
          participant,
          capability,
        } => Prepared::Observer(subscriptions::prepare_observer(
          rooms.clone(),
          sources.clone(),
          participant.clone(),
          capability.clone(),
          Some(id),
          true,
        )?),
        Lane::Attached { .. } => continue,
      };
      prepared.insert(id, subscription);
    }
    // The observation channel and its queued events must exist before we can
    // attach an input channel, regardless of the clients' reconnect order.
    for (&id, lane) in &self.lanes {
      if let Lane::Attached {
        source,
        capability,
        key,
      } = lane
      {
        let member = subscriptions::prepare_attachment(
          rooms.clone(),
          capability,
          source,
          key.clone(),
          Some(id),
        )?;
        prepared.insert(id, Prepared::Attached(member));
      }
    }
    rooms.state.lock().unwrap().restore_leases(&self.leases)?;
    *rooms.dormant.lock().unwrap() = prepared;
    Ok(())
  }

  fn validate(&self) -> io::Result<()> {
    if self.lanes.len() > super::MAX_CONNECTIONS {
      return Err(invalid("too many handoff lanes"));
    }
    let mut keys = BTreeSet::new();
    let mut capabilities = BTreeSet::new();
    for lane in self.lanes.values() {
      let key = match lane {
        Lane::Join { request, key } => {
          request.validate()?;
          if key.is_some() != (request.mode == Mode::Authority) {
            return Err(invalid("invalid handoff controller"));
          }
          key.as_ref()
        }
        Lane::Observer {
          sources,
          participant,
          capability,
        } => {
          crate::daemon::observer::validate(sources, &participant.agent_id)?;
          if !uuid(&participant.participant_id)
            || !uuid(capability)
            || !capabilities.insert(capability)
          {
            return Err(invalid("invalid handoff observer"));
          }
          None
        }
        Lane::Attached {
          source,
          key,
          capability,
        } => {
          source.validate()?;
          if !uuid(capability) {
            return Err(invalid("invalid handoff attachment"));
          }
          Some(key)
        }
      };
      if let Some(key) = key
        && (!uuid(key) || !keys.insert(key))
      {
        return Err(invalid("invalid handoff controller key"));
      }
    }
    Ok(())
  }
}

fn uuid(id: &str) -> bool {
  Uuid::try_parse(id).is_ok_and(|value| value.to_string() == id)
}

/// The transport calls this when a parked peer closed or the resumption
/// window elapsed. Drop outside the lock; normal membership Drop releases
/// turns, revokes observer attachments, and broadcasts a single departure.
pub(in crate::daemon) fn abandon(rooms: &Rooms, lane: Uuid) {
  let dormant = rooms.dormant.lock().unwrap().remove(&lane);
  drop(dormant);
}

pub(in crate::daemon) fn abandon_all(rooms: &Rooms) {
  let dormant = std::mem::take(&mut *rooms.dormant.lock().unwrap());
  drop(dormant);
}

pub(in crate::daemon) fn pause_expiry(rooms: &Rooms, paused: bool) {
  let mut state = rooms.state.lock().unwrap();
  state.expiry_paused = paused;
  state.timer.notify_one();
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::daemon::authority::{Command, Reply};
  use ctream_proto::{
    Target,
    live::{LiveAction, LiveEvent},
  };
  use std::time::Duration;

  fn source(id: &str) -> Room {
    Room::new(Target::X11(1), Some(id))
  }

  fn join_request(source: &Room, participant: &Participant, mode: Mode) -> Join {
    Join {
      room: source.clone(),
      participant: participant.clone(),
      subscribe: mode != Mode::Authority,
      mode,
    }
  }

  fn participant(label: &str) -> Participant {
    Participant {
      participant_id: Uuid::new_v4().to_string(),
      agent_id: label.to_owned(),
    }
  }

  fn transfer(rooms: &Rooms) -> Rooms {
    let bytes = serde_json::to_vec(&Checkpoint::capture(rooms).unwrap()).unwrap();
    let successor = Rooms::default();
    serde_json::from_slice::<Checkpoint>(&bytes)
      .unwrap()
      .restore(&successor)
      .unwrap();
    successor
  }

  #[test]
  fn attached_input_can_resume_first_and_delivers_ordered_events_to_the_dormant_observer() {
    let rooms = Rooms::default();
    let (obs_id, control_id, sdk_id) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let sources = vec![source("a"), source("b")];
    let viewer = participant("shared-label");
    let sdk = participant("shared-label");
    let capability = Uuid::new_v4().to_string();
    let _observer = subscriptions::prepare_observer(
      rooms.clone(),
      sources.clone(),
      viewer.clone(),
      capability.clone(),
      Some(obs_id),
      false,
    )
    .unwrap();
    let control =
      subscriptions::attach_lane(rooms.clone(), &capability, &sources[1], Some(control_id))
        .unwrap();
    let request = join_request(&sources[0], &sdk, Mode::Authority);
    let _sdk = join(rooms.clone(), request.clone(), Some(sdk_id)).unwrap();
    let Reply::Ok { value: Some(token) } = control.command(Command::Acquire { ttl_ms: 30_000 })
    else {
      panic!("turn")
    };
    let (_, reference, _) = control.turn_lease().unwrap();
    let expected_expiry = rooms.state.lock().unwrap().turn_event(&sources[0]);

    let successor = transfer(&rooms);
    let resumed = subscriptions::attach_lane(
      successor.clone(),
      &capability,
      &sources[1],
      Some(control_id),
    )
    .unwrap();
    assert_eq!(resumed.key, control.key);
    let (restored_token, restored_reference, _) = resumed.turn_lease().unwrap();
    assert_eq!(restored_token, token);
    assert_eq!(restored_reference, reference);
    assert_eq!(
      successor.state.lock().unwrap().turn_event(&sources[0]),
      expected_expiry
    );

    let (rival, _, _) = join(successor.clone(), request, Some(sdk_id)).unwrap();
    assert!(
      matches!(rival.command(Command::Acquire {ttl_ms:100}), Reply::Conflict{ owner_id:Some(id),.. } if id == viewer.participant_id)
    );
    assert!(matches!(
      resumed.command(Command::Begin {
        turn_id: Some(token.clone())
      }),
      Reply::Ok { .. }
    ));
    let action_id = Uuid::new_v4().to_string();
    resumed
      .publish_recorded(
        LiveAction::Click {
          point: [0.25, 0.75],
        },
        Some(action_id.clone()),
      )
      .unwrap();
    assert!(matches!(
      resumed.command(Command::Finish {}),
      Reply::Ok { .. }
    ));
    drop(rival);
    let (_owner, mut received, _) = subscriptions::subscribe(
      successor.clone(),
      sources.clone(),
      viewer.agent_id.clone(),
      Some(obs_id),
    )
    .unwrap();
    let (where_, action) = received.try_recv().unwrap();
    assert_eq!(where_, 1);
    assert!(
      matches!(action.as_ref(),LiveEvent::Action{action_id:Some(id),participant_id,..} if id==&action_id && participant_id==&viewer.participant_id)
    );
    let (where_, left) = received.try_recv().unwrap();
    assert_eq!(where_, 0);
    assert_eq!(
      left.as_ref(),
      &LiveEvent::Left {
        participant_id: sdk.participant_id
      }
    );
    assert!(
      received.try_recv().is_err(),
      "resume must not invent joins or another snapshot"
    );
    assert!(matches!(
      resumed.command(Command::Release { turn_id: token }),
      Reply::Ok { .. }
    ));
    assert!(successor.dormant.lock().unwrap().is_empty());
  }

  #[test]
  fn expired_turn_is_fenced_and_correctly_announced_before_observer_resumes() {
    let rooms = Rooms::default();
    let src = source("b");
    let c = Uuid::new_v4();
    let w = Uuid::new_v4();
    let driver = join_request(&src, &participant("sdk"), Mode::Authority);
    let watcher = join_request(&src, &participant("watch"), Mode::Turns);
    let (old, _, _) = join(rooms.clone(), driver.clone(), Some(c)).unwrap();
    let _watch = join(rooms.clone(), watcher.clone(), Some(w)).unwrap();
    let Reply::Ok { value: Some(token) } = old.command(Command::Acquire { ttl_ms: 10 }) else {
      panic!("turn")
    };
    pause_expiry(&rooms, true);
    let checkpoint = Checkpoint::capture(&rooms).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let successor = Rooms::default();
    checkpoint.restore(&successor).unwrap();
    let (next, _, _) = join(successor.clone(), driver, Some(c)).unwrap();
    assert!(
      matches!(next.command(Command::Begin{turn_id:Some(token)}),Reply::Conflict{code,..} if code=="stale_turn")
    );
    let (_watch, mut events, _) = join(successor.clone(), watcher, Some(w)).unwrap();
    assert_eq!(
      *events.try_recv().unwrap(),
      LiveEvent::Turn {
        holder: None,
        expires_at_ms: None
      }
    );
    assert!(events.try_recv().is_err());
  }

  #[test]
  fn unclaimed_lane_cleanup_revokes_turn_and_announces_departure_once() {
    let rooms = Rooms::default();
    let c = Uuid::new_v4();
    let w = Uuid::new_v4();
    let actor = participant("sdk");
    let src = source("a");
    let driver = join_request(&src, &actor, Mode::Authority);
    let watcher = join_request(&src, &participant("peer"), Mode::Turns);
    let (control, _, _) = join(rooms.clone(), driver, Some(c)).unwrap();
    let _watch = join(rooms.clone(), watcher.clone(), Some(w)).unwrap();
    assert!(matches!(
      control.command(Command::Acquire { ttl_ms: 20_000 }),
      Reply::Ok { .. }
    ));
    let successor = transfer(&rooms);
    abandon(&successor, c);
    abandon(&successor, c);
    let (_watch, mut events, _) = join(successor.clone(), watcher, Some(w)).unwrap();
    assert_eq!(
      *events.try_recv().unwrap(),
      LiveEvent::Turn {
        holder: None,
        expires_at_ms: None
      }
    );
    assert_eq!(
      *events.try_recv().unwrap(),
      LiveEvent::Left {
        participant_id: actor.participant_id
      }
    );
    assert!(events.try_recv().is_err());
  }

  #[test]
  fn legacy_members_and_active_gestures_block_the_cut() {
    let rooms = Rooms::default();
    let src = source("a");
    let driver = join_request(&src, &participant("sdk"), Mode::Authority);
    let legacy = join(rooms.clone(), driver.clone(), None).unwrap();
    assert_eq!(
      Checkpoint::capture(&rooms).err().unwrap().kind(),
      io::ErrorKind::WouldBlock
    );
    drop(legacy);
    let (member, _, _) = join(rooms.clone(), driver, Some(Uuid::new_v4())).unwrap();
    member.command(Command::Begin { turn_id: None });
    assert_eq!(
      Checkpoint::capture(&rooms).err().unwrap().kind(),
      io::ErrorKind::WouldBlock
    );
    member.command(Command::Finish {});
    assert!(Checkpoint::capture(&rooms).is_ok());
  }

  #[test]
  fn a_mismatched_transport_lane_cannot_rebind_another_participant() {
    let rooms = Rooms::default();
    let id = Uuid::new_v4();
    let original = join_request(&source("a"), &participant("editor"), Mode::Authority);
    let _member = join(rooms.clone(), original.clone(), Some(id)).unwrap();
    let desktop = Checkpoint::capture(&rooms).unwrap();
    let different = Opening::Join(join_request(
      &source("b"),
      &original.participant,
      Mode::Authority,
    ));
    assert!(
      desktop
        .validate_openings([(id, &different)].into_iter())
        .is_err()
    );
    assert!(desktop.validate_openings(std::iter::empty()).is_err());
    assert!(
      desktop
        .validate_openings([(id, &Opening::Join(original))].into_iter())
        .is_ok()
    );
  }
}
