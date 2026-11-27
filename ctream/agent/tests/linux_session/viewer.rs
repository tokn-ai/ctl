use super::host::{HostFixture, activity_details};
use ctream_proto::{
  DAEMON_VERSION, DISPLAY_VERSION, Target,
  live::{LiveAction, LiveEvent},
};
use std::{
  error::Error,
  io::{self, BufRead, BufReader, Write},
  process::{Child, ChildStdout, Stdio},
  thread,
  time::{Duration, Instant},
};
use x11rb::{
  connection::Connection as _,
  protocol::xproto::{ConnectionExt as _, KeyButMask},
};

struct Viewer {
  child: Child,
  output: BufReader<ChildStdout>,
}

impl Viewer {
  fn open(host: &HostFixture, display: &str, name: &str, window: Option<&str>) -> io::Result<Self> {
    let mut command = host.command();
    command.args([DAEMON_VERSION, "viewer-input", display, name]);
    if let Some(window) = window {
      command.arg(window);
    }
    let mut child = command
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .spawn()?;
    let output = BufReader::new(child.stdout.take().unwrap());
    let mut this = Self { child, output };
    let mut header = String::new();
    this.output.read_line(&mut header)?;
    assert!(
      header.trim() == "ready" || header.split_whitespace().count() == 2,
      "{header}"
    );
    Ok(this)
  }
  fn request(&mut self, message: &str) -> io::Result<String> {
    self
      .child
      .stdin
      .as_mut()
      .unwrap()
      .write_all(message.as_bytes())?;
    let mut reply = String::new();
    self.output.read_line(&mut reply)?;
    Ok(reply)
  }
  fn ok(&mut self, msg: &str) -> io::Result<()> {
    if msg.starts_with("batch ") {
      self.action_id(msg)?;
    } else {
      assert_eq!(self.request(msg)?, "ok\n", "{msg}");
    }
    Ok(())
  }
  fn action_id(&mut self, msg: &str) -> io::Result<String> {
    let reply = self.request(msg)?;
    let id = reply
      .trim_end()
      .strip_prefix("ok ")
      .expect("missing action receipt");
    assert_eq!(uuid::Uuid::parse_str(id).unwrap().to_string(), id);
    Ok(id.to_owned())
  }
  fn close(mut self) -> io::Result<()> {
    drop(self.child.stdin.take());
    assert!(self.child.wait()?.success());
    Ok(())
  }
}
impl Drop for Viewer {
  fn drop(&mut self) {
    drop(self.child.stdin.take());
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

#[test]
#[ignore = "requires the disposable X11 session in docker/ctream"]
fn native_viewers_fence_drags_turn_expiry_and_disconnect() -> Result<(), Box<dyn Error>> {
  let display = std::env::var("CTREAM_TEST_X11").unwrap_or_else(|_| ":98".into());
  let target = Target::parse(&display).unwrap();
  let host = HostFixture::new("viewer")?;
  let client = host.client()?;
  let window = client
    .windows(target)?
    .into_iter()
    .find(|w| w.title == "Ctream X11 fixture")
    .expect("fixture window");
  let (conn, screen) = x11rb::connect(Some(&display))?;
  let root = conn.setup().roots[screen].root;
  let pressed = || -> Result<bool, Box<dyn Error>> {
    Ok(
      conn
        .query_pointer(root)?
        .reply()?
        .mask
        .contains(KeyButMask::BUTTON1),
    )
  };
  let mut holder = Viewer::open(&host, &display, "viewer-one", None)?;
  let mut peer = Viewer::open(&host, &display, "viewer-two", Some(&window.id))?;
  let session = client.session(target, Some(window));
  let (mut observer, _subscription) = session.live_with_daemon("observer")?.split();
  let Some(LiveEvent::Snapshot { participants, .. }) = observer.next_event()? else {
    panic!("viewer room omitted its roster");
  };
  let peer_id = &participants
    .iter()
    .find(|p| p.agent_id == "viewer-two")
    .unwrap()
    .participant_id;
  assert!(matches!(
    observer.next_event()?,
    Some(LiveEvent::Turn { holder: None, .. })
  ));
  holder.ok("batch 2\nmove 10 10\nbutton 1 1\n")?;
  assert!(pressed()?);
  let blocked =
    serde_json::from_str::<serde_json::Value>(&peer.request("batch 1\nmove 2 2 80 60\n")?)?;
  assert_eq!(blocked["error"]["code"], "busy");
  let mut old = host
    .command()
    .args([DISPLAY_VERSION, "input", &display])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;
  old.stdin.take().unwrap().write_all(b"move 21 21\n")?;
  let old = old.wait_with_output()?;
  assert!(!old.status.success());
  assert!(String::from_utf8_lossy(&old.stderr).contains("busy"));
  // The competing view still has permission to capture this exact window.
  let observation = session.automation()?.screenshot()?;
  assert!(observation.size.iter().all(|&dimension| dimension > 0));
  holder.close()?;
  assert!(
    !pressed()?,
    "driver must release the held OS button before peer admission"
  );
  let receipt = peer.action_id("batch 1\nmove 3 3 80 60\n")?;
  let raw_event = observer.next_event()?;
  let Some(LiveEvent::Action {
    action: LiveAction::Move { .. },
    participant_id,
    action_id: Some(action_id),
  }) = raw_event
  else {
    panic!("expected the first accepted viewer action, got {raw_event:?}");
  };
  assert_eq!(&participant_id, peer_id);
  assert_eq!(receipt, action_id);
  let journal = activity_details(host.state())?;
  let accepted = journal
    .iter()
    .find(|row| row["action_id"] == action_id)
    .expect("raw overlay came from the journal");
  assert_eq!(accepted["operation"], "move");
  assert_eq!(accepted["status"], "succeeded");
  assert_eq!(accepted["session_id"], participant_id);
  let busy: Vec<_> = journal
    .iter()
    .filter(|row| row["refusal_code"] == "busy")
    .collect();
  assert_eq!(
    busy.len(),
    2,
    "named viewer and retained raw input both submit to the daemon: {journal:?}"
  );
  for row in &busy {
    assert_eq!(row["operation"], "move");
    assert_eq!(row["status"], "failed");
    assert_eq!(row["owner_id"], blocked["error"]["owner_id"]);
  }
  assert!(
    busy
      .iter()
      .any(|row| row["session_id"] == *peer_id && row["action_id"] == blocked["action_id"])
  );
  assert!(busy.iter().any(|row| {
    row["agent_id"]
      .as_str()
      .is_some_and(|name| name.contains('#'))
  }));

  let mut holder = Viewer::open(&host, &display, "viewer-one", None)?;
  holder.ok("take-turn 350\n")?;
  holder.ok("batch turn 2\nmove 12 12\nbutton 1 1\n")?;
  assert!(pressed()?);
  let until = Instant::now() + Duration::from_secs(5);
  while pressed()? && Instant::now() < until {
    thread::sleep(Duration::from_millis(10));
  }
  assert!(
    !pressed()?,
    "idle socket must release OS input at the turn deadline"
  );
  // The OS button transitions just before the worker acknowledges cleanup.
  // Probe admission without duplicating an input action during that interval.
  super::wait_for("daemon handoff after native cleanup", || {
    let reply = peer.request("take-turn 10000\n")?;
    if reply == "ok\n" {
      return Ok(Some(()));
    }
    let reply: serde_json::Value = serde_json::from_str(&reply)?;
    assert_eq!(reply["error"]["code"], "busy");
    Ok(None)
  })?;
  peer.ok("batch turn 1\nmove 4 4 80 60\n")?;
  peer.ok("release-turn\n")?;
  let stale =
    serde_json::from_str::<serde_json::Value>(&holder.request("batch turn 1\nmove 111 111\n")?)?;
  assert_eq!(stale["error"]["code"], "stale_turn");
  let pointer = conn.query_pointer(root)?.reply()?;
  assert_eq!((pointer.root_x, pointer.root_y), (12, 12));
  holder.ok("batch 1\nmove 15 15\n")?;
  holder.ok("take-turn 10000\n")?;
  holder.ok("release-turn\n")?;
  peer.ok("batch 1\nmove 5 5 80 60\n")?;
  holder.close()?;
  peer.close()?;
  Ok(())
}

#[test]
#[ignore = "requires the disposable X11 session in docker/ctream"]
fn raw_viewer_lanes_share_identity_without_merging_matching_labels() -> Result<(), Box<dyn Error>> {
  let host = HostFixture::new("viewer-identity")?;
  let client = host.client()?;
  let display = std::env::var("CTREAM_TEST_X11").unwrap_or_else(|_| ":98".into());
  let target = Target::parse(&display)?;
  let window = client
    .windows(target)?
    .into_iter()
    .find(|w| w.title == "Ctream X11 fixture")
    .unwrap();
  let session = client.session(target, Some(window));
  let (mut first, first_watch) = session.viewer_observer("viewer")?.split();
  let a_key = first.viewer_attachment()?;
  let Some(LiveEvent::Snapshot {
    self_id: a,
    participants,
  }) = first.next_event()?
  else {
    panic!("missing first roster")
  };
  assert_eq!(participants.len(), 1);
  assert!(matches!(
    first.next_event()?,
    Some(LiveEvent::Turn { holder: None, .. })
  ));
  let (mut second, _second_watch) = session.viewer_observer("viewer")?.split();
  let b_key = second.viewer_attachment()?;
  let Some(LiveEvent::Snapshot {
    self_id: b,
    participants,
  }) = second.next_event()?
  else {
    panic!("missing second roster")
  };
  assert_ne!(a, b);
  assert_eq!(participants.len(), 2);
  assert!(participants.iter().all(|p| p.agent_id == "viewer"));
  assert_ne!(a_key.as_secret(), b_key.as_secret());
  assert_ne!(a, a_key.as_secret());
  assert!(matches!(
    second.next_event()?,
    Some(LiveEvent::Turn { holder: None, .. })
  ));
  assert!(
    matches!(first.next_event()?,Some(LiveEvent::Joined{participant}) if participant.participant_id==b)
  );
  let mut lhs = session.viewer_control("viewer", None, Some(&a_key))?;
  let mut rhs = session.viewer_control("viewer", None, Some(&b_key))?;
  lhs.move_pointer(2, 3, [80, 60])?;
  let id = lhs.last_action_id().unwrap();
  for read in [&mut first, &mut second] {
    assert!(
      matches!(read.next_event()?,Some(LiveEvent::Action {participant_id, action_id:Some(receipt),..}) if participant_id==a && receipt==id)
    );
  }
  rhs.toggle_turn()?;
  assert!(
    matches!(first.next_event()?,Some(LiveEvent::Turn {holder:Some(holder),..}) if holder.participant_id==b)
  );
  assert!(
    matches!(second.next_event()?,Some(LiveEvent::Turn{holder:Some(holder),..}) if holder.participant_id==b)
  );
  let busy = lhs.move_pointer(4, 5, [80, 60]).unwrap_err();
  assert!(busy.to_string().contains(&b));
  rhs.toggle_turn()?;
  lhs.finish_bounded()?;
  // A reopened control lane still attaches to its live observer. Closing that
  // observer retires the capability and makes the selected participant leave.
  let reopened = session.viewer_control("viewer", None, Some(&a_key))?;
  reopened.finish_bounded()?;
  drop(first_watch);
  drop(first);
  assert!(matches!(
    second.next_event()?,
    Some(LiveEvent::Turn { holder: None, .. })
  ));
  assert!(
    matches!(second.next_event()?,Some(LiveEvent::Left {participant_id}) if participant_id==a)
  );
  assert!(
    session
      .viewer_control("viewer", None, Some(&a_key))
      .is_err()
  );
  rhs.finish_bounded()?;
  Ok(())
}
