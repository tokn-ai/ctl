use super::*;
use ctream_agent::daemon::SourceObserver;
use ctream_client::sdk::Mutation;
use ctream_proto::live::LiveEvent;
use tokio::time::timeout;

#[test]
#[ignore = "requires the disposable X11 session in docker/ctream"]
fn observer_can_control_a_watched_window_under_its_existing_identity() -> Result<(), Box<dyn Error>>
{
  let fixture = HostFixture::new("observer")?;
  let status = fixture
    .command_for(&std::env::current_exe()?)
    .args([
      "--exact",
      "attribution::observer_control::observer_child",
      "--nocapture",
    ])
    .env("CTREAM_TEST_ATTACHED_OBSERVER", "1")
    .status()?;
  assert!(status.success());
  Ok(())
}

#[tokio::test]
async fn observer_child() -> Result<(), Box<dyn Error>> {
  if std::env::var_os("CTREAM_TEST_ATTACHED_OBSERVER").is_none() {
    return Ok(());
  }
  let display = std::env::var("CTREAM_TEST_X11").unwrap_or_else(|_| ":98".into());
  let target = Target::parse(&display)?;
  let (conn, screen) = x11rb::connect(Some(&display))?;
  let screen = &conn.setup().roots[screen];
  let (_, first) = fixture_window(&conn, screen, target, 10, b"Observer control A")?;
  let (xid, second) = fixture_window(&conn, screen, target, 245, b"Observer control B")?;
  let sources = [(target, Some(first)), (target, Some(second.clone()))];
  let mut observer = SourceObserver::connect(Path::new(AGENT), "multi-viewer", &sources).await?;
  let wait = Duration::from_secs(5);
  let mut identity = String::new();
  for (_, source) in &sources {
    let snapshot = timeout(wait, observer.next_event()).await??.unwrap();
    assert_eq!(&snapshot.window_id, source);
    let LiveEvent::Snapshot {
      self_id,
      participants,
    } = snapshot.event
    else {
      panic!("expected roster")
    };
    assert_eq!(participants.len(), 1);
    if identity.is_empty() {
      identity = self_id;
    } else {
      assert_eq!(self_id, identity);
    }
    assert!(matches!(
      timeout(wait, observer.next_event()).await??.unwrap().event,
      LiveEvent::Turn { holder: None, .. }
    ));
  }
  let retained_attachment = observer.attachment().to_owned();
  let mut control = observer.control(target, Some(&second)).await?;
  let token = control.acquire(10_000).await?;
  let mut notifications = HashSet::new();
  for _ in &sources {
    let turn = timeout(wait, observer.next_event()).await??.unwrap();
    let LiveEvent::Turn {
      holder: Some(holder),
      ..
    } = turn.event
    else {
      panic!("expected turn; attachment must not add roster entries")
    };
    assert_eq!(holder.participant_id, identity);
    notifications.insert(turn.window_id.unwrap());
  }
  assert_eq!(notifications.len(), 2);
  let action_id = control
    .execute(
      Some(&token),
      Mutation::PressKeys {
        keys: vec!["x".into()],
      },
    )
    .await?;
  let confirmation = timeout(wait, observer.next_event()).await??.unwrap();
  assert_eq!(confirmation.window_id.as_ref(), Some(&second));
  let LiveEvent::Action {
    participant_id,
    action_id: Some(recorded),
    ..
  } = confirmation.event
  else {
    panic!("expected confirmed input")
  };
  assert_eq!(participant_id, identity);
  assert_eq!(recorded, action_id);
  let state = std::env::var_os("XDG_STATE_HOME").unwrap();
  let records = activity_details(Path::new(&state))?;
  assert_eq!(records.len(), 1);
  assert_eq!(records[0]["session_id"], identity);
  assert_eq!(records[0]["action_id"], action_id);
  assert_eq!(records[0]["status"], "succeeded");
  wait_for("physical input under observer's identity", || {
    while let Some(event) = conn.poll_for_event()? {
      if let Event::KeyPress(key) = event {
        assert_eq!(key.event, xid);
        return Ok(Some(()));
      }
    }
    Ok(None)
  })?;

  // A second participant witnesses the entire first session leave. Its own
  // input lane can then acquire the desktop without waiting for the old TTL.
  let mut witness = SourceObserver::connect(Path::new(AGENT), "witness", &sources[1..]).await?;
  let roster = timeout(wait, witness.next_event()).await??.unwrap();
  let LiveEvent::Snapshot { participants, .. } = roster.event else {
    panic!("expected roster")
  };
  assert_eq!(participants.len(), 2);
  assert!(matches!(
    timeout(wait, witness.next_event()).await??.unwrap().event,
    LiveEvent::Turn {
      holder: Some(_),
      ..
    }
  ));
  drop(observer);
  let mut left = false;
  let mut released = false;
  for _ in 0..2 {
    match timeout(wait, witness.next_event()).await??.unwrap().event {
      LiveEvent::Left { participant_id } => {
        assert_eq!(participant_id, identity);
        left = true;
      }
      LiveEvent::Turn { holder: None, .. } => released = true,
      other => panic!("unexpected witness event {other:?}"),
    }
  }
  assert!(left && released);
  let expired_source = second.clone();
  let reattached = tokio::task::spawn_blocking(move || {
    ctream_agent::daemon::Authority::attach(target, Some(&expired_source), &retained_attachment)
      .map(drop)
  })
  .await?;
  assert!(
    reattached.is_err(),
    "a retired secret reattached its old identity"
  );
  assert!(
    control
      .execute(
        Some(&token),
        Mutation::PressKeys {
          keys: vec!["y".into()]
        }
      )
      .await
      .is_err()
  );
  let mut next = witness.control(target, Some(&second)).await?;
  let new_turn = next.acquire(10_000).await?;
  next.release(&new_turn).await?;
  next.close().await?;
  Ok(())
}
