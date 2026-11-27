use super::{Membership, Rooms, State, broadcast};
use crate::daemon::{
  Room,
  authority::{Command, Reply},
  invalid,
};
use ctream_proto::live::{LiveEvent, MAX_TURN_MS, Participant};
use std::{
  io,
  time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Domain {
  Linux,
  Windows,
  Macos,
}
impl Room {
  pub(in crate::daemon) fn shares_desktop(&self, other: &Self) -> bool {
    self.domain() == other.domain()
  }

  fn domain(&self) -> Domain {
    if self.target == "macos" {
      Domain::Macos
    } else if self.target.starts_with("windows") {
      Domain::Windows
    } else {
      // SSH cannot establish which X server is XWayland on a given compositor.
      Domain::Linux
    }
  }
}
pub(super) struct Lease {
  key: String,
  token: String,
  owner: Participant,
  end: Instant,
  wall_end: u64,
}
pub(super) struct Gesture {
  key: String,
  room: Room,
  owner: Participant,
  id: String,
  open: bool,
  implicit: bool,
  pins: u32,
}
#[derive(Default)]
pub(super) struct Turns {
  lease: Option<Lease>,
  gesture: Option<Gesture>,
}
fn busy(owner: &Participant) -> Reply {
  Reply::Conflict {
    code: "busy".into(),
    owner_id: Some(owner.participant_id.clone()),
  }
}
fn stale() -> Reply {
  Reply::Conflict {
    code: "stale_turn".into(),
    owner_id: None,
  }
}

impl State {
  pub(super) fn turn_event(&self, room: &Room) -> LiveEvent {
    let lease = self
      .turns
      .get(&room.domain())
      .and_then(|turn| turn.lease.as_ref());
    LiveEvent::Turn {
      holder: lease.map(|l| l.owner.clone()),
      expires_at_ms: lease.map(|l| l.wall_end),
    }
  }
  fn notify_turn(&mut self, domain: Domain) {
    self.timer.notify_one();
    let lease = self.turns.get(&domain).and_then(|turn| turn.lease.as_ref());
    let event = LiveEvent::Turn {
      holder: lease.map(|l| l.owner.clone()),
      expires_at_ms: lease.map(|l| l.wall_end),
    };
    for (room, members) in &mut self.rooms {
      if room.domain() == domain {
        broadcast(members, event.clone())
      }
    }
  }
  pub(super) fn expire(&mut self, now: Instant) {
    let mut changed = Vec::new();
    for (&domain, turn) in &mut self.turns {
      if turn.lease.as_ref().is_some_and(|lease| lease.end <= now) {
        turn.lease = None;
        changed.push(domain);
      }
    }
    for domain in changed {
      self.notify_turn(domain)
    }
  }
  pub(super) fn close_authority(&mut self, room: &Room, key: &str) {
    let domain = room.domain();
    let turn = self.turns.entry(domain).or_default();
    if let Some(gesture) = &mut turn.gesture
      && gesture.key == key
    {
      gesture.open = false;
      if gesture.pins == 0 {
        turn.gesture = None
      }
    }
    if turn.lease.as_ref().is_some_and(|lease| lease.key == key) {
      turn.lease = None;
      self.notify_turn(domain);
    }
  }
}

pub(super) async fn expire_leases(rooms: Rooms) {
  let timer = rooms.state.lock().unwrap().timer.clone();
  loop {
    let deadline = {
      let mut state = rooms.state.lock().unwrap();
      state.expire(Instant::now());
      state
        .turns
        .values()
        .filter_map(|turn| turn.lease.as_ref().map(|lease| lease.end))
        .min()
    };
    if let Some(deadline) = deadline {
      tokio::select! {_=timer.notified()=>{},_=tokio::time::sleep_until(deadline)=>{}}
    } else {
      timer.notified().await;
    }
  }
}

impl super::Desktop {
  /// Close admission without invalidating the driver of an already-admitted
  /// action. The same mutex guards the predicate and the condvar wait.
  pub(super) fn stop_admitting(&self, key: &str) {
    if let Some(route) = self.state.lock().unwrap().controllers.get_mut(key) {
      route.accepting = false;
      self.available.notify_all();
    }
  }
}

impl Membership {
  pub(super) fn command(&self, command: Command) -> Reply {
    self.command_locked(&mut self.rooms.state.lock().unwrap(), command)
  }

  // Only independent implicit gestures may wait for one another. Explicit
  // reservations and held buttons report their owner immediately.
  pub(super) fn admit(&self, turn_id: Option<String>) -> Reply {
    let deadline = std::time::Instant::now() + crate::daemon::IO_TIMEOUT;
    let mut state = self.rooms.state.lock().unwrap();
    loop {
      let reply = self.command_locked(
        &mut state,
        Command::Begin {
          turn_id: turn_id.clone(),
        },
      );
      let Reply::Pending { owner_id } = reply else {
        return reply;
      };
      let remaining = deadline.saturating_duration_since(std::time::Instant::now());
      if remaining.is_zero() {
        return Reply::Conflict {
          code: "busy".into(),
          owner_id: Some(owner_id),
        };
      }
      (state, _) = self.rooms.available.wait_timeout(state, remaining).unwrap();
    }
  }

  fn command_locked(&self, state: &mut State, command: Command) -> Reply {
    let key = self.key.as_ref().expect("controller key");
    if matches!(command, Command::Acquire { .. } | Command::Begin { .. }) {
      let connected = state
        .controllers
        .get(key)
        .is_some_and(|route| route.accepting);
      let revoked = self
        .attachment
        .as_ref()
        .is_some_and(|lane| !state.observers.contains_key(&lane.capability));
      if !connected || revoked {
        return stale();
      }
    }
    let now = Instant::now();
    state.expire(now);
    let domain = self.room.domain();
    let turn = state.turns.entry(domain).or_default();
    match command {
      Command::Acquire { ttl_ms } => {
        if ttl_ms == 0 || ttl_ms > MAX_TURN_MS {
          return Reply::invalid("invalid turn duration");
        }
        if let Some(lease) = &turn.lease {
          return busy(&lease.owner);
        }
        if let Some(gesture) = &turn.gesture {
          return busy(&gesture.owner);
        }
        let token = Uuid::new_v4().to_string();
        let wall_now = SystemTime::now()
          .duration_since(UNIX_EPOCH)
          .unwrap_or_default()
          .as_millis() as u64;
        turn.lease = Some(Lease {
          key: key.clone(),
          token: token.clone(),
          owner: self.participant.clone(),
          end: now + Duration::from_millis(ttl_ms),
          wall_end: wall_now.saturating_add(ttl_ms),
        });
        state.notify_turn(domain);
        Reply::value(token)
      }
      Command::Release { turn_id } => {
        if !turn
          .lease
          .as_ref()
          .is_some_and(|lease| lease.key == *key && lease.token == turn_id)
        {
          return stale();
        }
        turn.lease = None;
        state.notify_turn(domain);
        Reply::done()
      }
      Command::Begin { turn_id } => {
        let implicit = turn_id.is_none();
        match (&turn.lease, turn_id) {
          (Some(lease), Some(id)) if lease.key == *key && lease.token == id => {}
          (_, Some(_)) => return stale(),
          (Some(lease), None) if lease.key == *key => return stale(),
          (Some(lease), None) => return busy(&lease.owner),
          (None, None) => {}
        }
        if let Some(gesture) = &mut turn.gesture {
          // A native drag can span requests. Only its original connection
          // may continue a pinned gesture, after the lease check above.
          if gesture.key == *key && !gesture.open && gesture.pins != 0 {
            gesture.open = true;
            return Reply::value(gesture.id.clone());
          }
          return if gesture.open && gesture.implicit && gesture.key != *key {
            Reply::Pending {
              owner_id: gesture.owner.participant_id.clone(),
            }
          } else {
            busy(&gesture.owner)
          };
        }
        let id = Uuid::new_v4().to_string();
        turn.gesture = Some(Gesture {
          key: key.clone(),
          owner: self.participant.clone(),
          id: id.clone(),
          room: self.room.clone(),
          open: true,
          implicit,
          pins: 0,
        });
        Reply::value(id)
      }
      Command::Finish {} => {
        let Some(gesture) = &mut turn.gesture else {
          return stale();
        };
        if gesture.key != *key || !gesture.open {
          return stale();
        }
        gesture.open = false;
        self.rooms.available.notify_all();
        if gesture.pins == 0 {
          turn.gesture = None
        }
        Reply::done()
      }
      _ => Reply::invalid("invalid turn command"),
    }
  }
}

pub(super) struct Pin {
  rooms: Rooms,
  room: Room,
  key: String,
  admitted: Option<String>,
}
impl Pin {
  pub(super) fn attach(rooms: Rooms, room: Room, key: String) -> io::Result<Self> {
    if key.len() != 36
      || !rooms
        .state
        .lock()
        .unwrap()
        .controllers
        .get(&key)
        .is_some_and(|route| route.source == room)
    {
      return Err(invalid("invalid native driver delegation"));
    }
    Ok(Self {
      rooms,
      room,
      key,
      admitted: None,
    })
  }
  pub(super) fn begin(&mut self) -> Reply {
    if self.admitted.is_some() {
      return Reply::invalid("driver already admitted");
    }
    let mut state = self.rooms.state.lock().unwrap();
    let Some(gesture) = state
      .turns
      .get_mut(&self.room.domain())
      .and_then(|turn| turn.gesture.as_mut())
    else {
      return stale();
    };
    if gesture.key != self.key || gesture.room != self.room || !gesture.open {
      return stale();
    }
    gesture.pins += 1;
    self.admitted = Some(gesture.id.clone());
    Reply::value(gesture.id.clone())
  }
  pub(super) fn finish(&mut self) -> Reply {
    let Some(id) = self.admitted.take() else {
      return stale();
    };
    let mut state = self.rooms.state.lock().unwrap();
    let turn = state
      .turns
      .get_mut(&self.room.domain())
      .expect("pinned domain");
    let gesture = turn.gesture.as_mut().expect("pinned gesture");
    debug_assert_eq!(gesture.id, id);
    gesture.pins -= 1;
    if !gesture.open && gesture.pins == 0 {
      turn.gesture = None;
      self.rooms.available.notify_all();
    }
    Reply::done()
  }
}
impl Drop for Pin {
  fn drop(&mut self) {
    if self.admitted.is_some() {
      self.finish();
    }
  }
}
