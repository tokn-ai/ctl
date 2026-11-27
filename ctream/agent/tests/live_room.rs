use ctream_agent::daemon::Publisher;
use ctream_proto::{
  LIVE_VERSION, Target,
  live::{LiveAction, LiveEvent, Participant},
};
use std::{
  io::{self, BufRead, BufReader, Write},
  process::{Child, Command, Stdio},
  sync::mpsc::{self, RecvTimeoutError},
  thread,
  time::{Duration, Instant},
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(6);

struct Watcher {
  child: Child,
  events: mpsc::Receiver<LiveEvent>,
  me: Participant,
}

impl Watcher {
  fn open(target: Target, window: &str, name: &str) -> Self {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ctream-agent"))
      .args([LIVE_VERSION, "live-room", &target.to_string(), name, window])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .spawn()
      .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, events) = mpsc::channel();
    thread::spawn(move || {
      for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if tx.send(serde_json::from_str(&line).unwrap()).is_err() {
          break;
        }
      }
    });
    Self {
      child,
      events,
      me: Participant {
        agent_id: name.into(),
        participant_id: String::new(),
      },
    }
  }

  fn snapshot(&mut self) -> Vec<Participant> {
    let LiveEvent::Snapshot {
      self_id,
      participants,
    } = self.next()
    else {
      panic!("expected initial snapshot");
    };
    self.me.participant_id = self_id;
    assert!(participants.contains(&self.me));
    participants
  }

  fn next(&self) -> LiveEvent {
    self
      .events
      .recv_timeout(WAIT)
      .expect("expected live room event")
  }

  fn close(&mut self) {
    drop(self.child.stdin.take());
    let deadline = Instant::now() + WAIT;
    loop {
      if let Some(status) = self.child.try_wait().unwrap() {
        assert!(status.success());
        return;
      }
      assert!(Instant::now() < deadline, "live-room did not exit on EOF");
      thread::sleep(Duration::from_millis(10));
    }
  }
}

impl Drop for Watcher {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

#[test]
fn independent_processes_share_only_the_exact_stream_until_eof() -> io::Result<()> {
  let display = Uuid::new_v4().as_fields().0;
  let target = Target::X11(display);
  let mut a = Watcher::open(target, "x11-1", "viewer");
  assert_eq!(a.snapshot(), [a.me.clone()]);
  let mut b = Watcher::open(target, "x11-1", "viewer");
  let peers = b.snapshot();
  assert_eq!(peers.len(), 2);
  assert!(peers.contains(&a.me));
  assert_ne!(a.me.participant_id, b.me.participant_id);
  assert_eq!(
    a.next(),
    LiveEvent::Joined {
      participant: b.me.clone()
    }
  );

  let mut other = Watcher::open(target, "x11-2", "viewer");
  assert_eq!(other.snapshot(), [other.me.clone()]);
  let mut other_display = Watcher::open(Target::X11(display.wrapping_add(1)), "x11-1", "viewer");
  assert_eq!(other_display.snapshot(), [other_display.me.clone()]);
  a.close();
  assert_eq!(
    b.next(),
    LiveEvent::Left {
      participant_id: a.me.participant_id.clone()
    }
  );

  let id = Uuid::new_v4().to_string();
  let mut sdk = Publisher::connect(target, Some("x11-1"), "worker", &id)?;
  assert_eq!(
    b.next(),
    LiveEvent::Joined {
      participant: Participant {
        participant_id: id.clone(),
        agent_id: "worker".into()
      }
    }
  );
  let action = LiveAction::Click {
    point: [0.25, 0.75],
  };
  // Receiving a remote event must not lose a partial outgoing NDJSON action.
  let pointer = LiveAction::PointerDown { point: [0.0, 1.0] };
  let parts = serde_json::to_vec(&pointer)?;
  b.child.stdin.as_mut().unwrap().write_all(&parts[..8])?;
  b.child.stdin.as_mut().unwrap().flush()?;
  sdk.publish(&action)?;
  assert_eq!(
    b.next(),
    LiveEvent::Action {
      action_id: None,
      participant_id: id.clone(),
      action
    }
  );
  drop(sdk);
  assert_eq!(b.next(), LiveEvent::Left { participant_id: id });

  let input = b.child.stdin.as_mut().unwrap();
  input.write_all(&parts[8..])?;
  input.write_all(b"\n")?;
  input.flush()?;
  assert_eq!(
    b.next(),
    LiveEvent::Action {
      action_id: None,
      participant_id: b.me.participant_id.clone(),
      action: pointer
    }
  );
  assert!(matches!(
    other.events.recv_timeout(Duration::from_millis(100)),
    Err(RecvTimeoutError::Timeout)
  ));
  assert!(matches!(
    other_display.events.try_recv(),
    Err(mpsc::TryRecvError::Empty)
  ));
  b.close();
  other.close();
  other_display.close();
  Ok(())
}

#[tokio::test]
async fn one_desktop_connection_watches_multiple_sources_with_one_participant_id() -> io::Result<()>
{
  use ctream_agent::daemon::SourceObserver;
  use std::path::Path;
  use tokio::task::spawn_blocking;
  use tokio::time::timeout;
  let display = Target::X11(Uuid::new_v4().as_fields().0);
  let sources = [
    (display, Some("x11-a".into())),
    (display, Some("x11-b".into())),
  ];
  let agent = Path::new(env!("CARGO_BIN_EXE_ctream-agent"));
  let mut observer = SourceObserver::connect(agent, "observer", &sources).await?;
  let mut participant = None;
  for (target, window) in &sources {
    let snapshot = timeout(WAIT, observer.next_event()).await??.unwrap();
    assert_eq!((snapshot.target, &snapshot.window_id), (*target, window));
    let LiveEvent::Snapshot {
      self_id,
      participants,
    } = snapshot.event
    else {
      panic!("missing source roster")
    };
    assert_eq!(
      participants,
      [Participant {
        agent_id: "observer".into(),
        participant_id: self_id.clone()
      }]
    );
    if let Some(expected) = participant.replace(self_id.clone()) {
      assert_eq!(self_id, expected);
    }
    let turn = timeout(WAIT, observer.next_event()).await??.unwrap();
    assert_eq!(turn.window_id, *window);
    assert!(matches!(turn.event, LiveEvent::Turn { .. }));
  }
  let participant = participant.unwrap();
  let mut watchers = Vec::new();
  for (_, window) in &sources {
    let peer_window = window.clone().unwrap();
    let (watcher, roster) = spawn_blocking(move || {
      let mut watcher = Watcher::open(display, &peer_window, "peer");
      let roster = watcher.snapshot();
      (watcher, roster)
    })
    .await?;
    assert!(roster.iter().any(|p| p.participant_id == participant));
    let joined = timeout(WAIT, observer.next_event()).await??.unwrap();
    assert_eq!(joined.window_id, *window);
    assert_eq!(
      joined.event,
      LiveEvent::Joined {
        participant: watcher.me.clone()
      }
    );
    watchers.push(watcher);
  }
  drop(observer);
  // Windows needs the observer's I/O driver to finish closing the named pipe
  // before the daemon can notify the waiting peers.
  spawn_blocking(move || {
    let left = LiveEvent::Left {
      participant_id: participant,
    };
    for mut watcher in watchers {
      assert_eq!(watcher.next(), left);
      watcher.close();
    }
  })
  .await?;
  Ok(())
}
