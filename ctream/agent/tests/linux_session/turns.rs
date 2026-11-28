use super::*;

fn request(client: &mut NamedClient, body: Value) -> io::Result<Value> {
  client.send(body)?;
  client.receive()
}

fn success(client: &mut NamedClient, body: Value) -> io::Result<Value> {
  let reply = request(client, body)?;
  assert_eq!(reply["ok"], true, "{reply}");
  Ok(reply["result"].clone())
}

fn acquire(client: &mut NamedClient, ttl_ms: u64) -> io::Result<String> {
  let result = success(client, json!({"operation":"acquire_turn","ttl_ms":ttl_ms}))?;
  Ok(
    result["turn_id"]
      .as_str()
      .expect("missing private turn ID")
      .to_owned(),
  )
}

fn action_id(reply: &Value) -> String {
  let id = reply["action_id"]
    .as_str()
    .expect("mutation reply has a public action ID");
  Uuid::parse_str(id).unwrap();
  id.to_owned()
}

fn denied_action(client: &mut NamedClient, body: Value, code: &str) -> io::Result<Value> {
  let reply = request(client, body)?;
  assert_eq!(reply["ok"], false, "{reply}");
  assert_eq!(reply["code"], code, "{reply}");
  Ok(reply)
}

fn accepted_action(client: &mut NamedClient, body: Value) -> io::Result<String> {
  let reply = request(client, body)?;
  assert_eq!(reply["ok"], true, "{reply}");
  assert_eq!(reply["result"], Value::Null);
  Ok(action_id(&reply))
}

fn free_event() -> Value {
  json!({"type":"turn","holder":null,"expires_at_ms":null})
}

fn held_event(observer: &mut NamedClient, id: &Value) -> io::Result<()> {
  let event = observer.receive()?;
  assert_eq!(event["type"], "turn");
  assert_eq!(
    event["holder"],
    json!({"participant_id":id,"agent_id":"writer"})
  );
  assert!(event["expires_at_ms"].as_u64().unwrap() > 0);
  assert_eq!(
    event.as_object().unwrap().len(),
    3,
    "turn notification must not expose a token"
  );
  Ok(())
}

fn writer_id(snapshot: &Value) -> Value {
  assert_eq!(snapshot["type"], "snapshot");
  snapshot["participants"]
    .as_array()
    .unwrap()
    .iter()
    .find(|p| p["agent_id"] == "writer")
    .unwrap()["participant_id"]
    .clone()
}

fn input_id(observer: &mut NamedClient, participant: &Value) -> Result<String, Box<dyn Error>> {
  let event = observer.receive()?;
  assert_eq!(event["participant_id"], *participant);
  assert_eq!(event["action"], json!({"operation":"press_keys"}));
  assert_eq!(event["type"], "action");
  assert_eq!(event.as_object().unwrap().len(), 4);
  let id = event["action_id"].as_str().unwrap();
  Uuid::parse_str(id)?;
  Ok(id.to_owned())
}

#[test]
#[ignore = "requires the disposable X11 session and sibling CLI in docker/ctream"]
fn desktop_turns_fence_input_across_windows_and_correlate_audit_events()
-> Result<(), Box<dyn Error>> {
  let fixture = HostFixture::new("turns")?;
  let host = fixture.state();
  let callers = [fixture.caller("a")?, fixture.caller("b")?];
  let display = std::env::var("CTREAM_TEST_X11").unwrap_or_else(|_| ":98".to_owned());
  let target = Target::parse(&display)?;
  let (conn, screen) = x11rb::connect(Some(&display))?;
  let screen = &conn.setup().roots[screen];
  let (xid, win_a) = fixture_window(&conn, screen, target, 10, b"Turn input A")?;
  let (_, win_b) = fixture_window(&conn, screen, target, 270, b"Turn input B")?;
  let open = |caller, window, command, label| {
    NamedClient::open(
      caller,
      host,
      &display,
      window,
      command,
      Some(("1.0.7", label)),
    )
  };
  let mut a = open(&callers[0], &win_a, "session", "writer")?;
  let mut b = open(&callers[1], &win_b, "session", "writer")?;
  let mut legacy = NamedClient::open(
    &callers[1],
    host,
    &display,
    &win_b,
    "session",
    Some(("1.0.3", "legacy")),
  )?;
  for client in [&mut a, &mut b, &mut legacy] {
    assert_eq!(
      success(client, json!({"operation":"capabilities"}))?["input"],
      true
    );
  }
  let mut watch_a = open(&callers[0], &win_a, "watch", "observer")?;
  let mut watch_b = open(&callers[1], &win_b, "watch", "observer")?;
  let id_a = writer_id(&watch_a.receive()?);
  let id_b = writer_id(&watch_b.receive()?);
  assert_ne!(id_a, id_b);
  for observer in [&mut watch_a, &mut watch_b] {
    assert_eq!(observer.receive()?, free_event());
  }

  // Each observer watches a different window. Both see the same desktop holder.
  let token = acquire(&mut a, 30_000)?;
  for observer in [&mut watch_a, &mut watch_b] {
    held_event(observer, &id_a)?;
  }
  let denied = denied_action(&mut b, json!({"operation":"acquire_turn"}), "busy")?;
  assert_eq!(denied["owner_id"], id_a);
  let denied = denied_action(
    &mut b,
    json!({"operation":"press_keys","keys":["d"]}),
    "busy",
  )?;
  assert_eq!(denied["owner_id"], id_a);
  let mut denials = vec![(action_id(&denied), "busy", id_b.clone())];
  let denied = request(&mut legacy, json!({"operation":"press_keys","keys":["e"]}))?;
  assert_eq!(denied["ok"], false);
  assert!(
    denied["error"].as_str().unwrap().contains("busy"),
    "{denied}"
  );
  let press =
    |key: &str, token: &str| json!({"operation":"press_keys","keys":[key],"turn_id":token});
  let denied = denied_action(&mut b, press("f", &token), "stale_turn")?;
  denials.push((action_id(&denied), "stale_turn", id_b.clone()));
  assert_eq!(
    success(&mut b, json!({"operation":"screenshot"}))?["mime_type"],
    "image/png"
  );

  let mut ids = Vec::new();
  for key in ["a", "b"] {
    let accepted = accepted_action(&mut a, press(key, &token))?;
    assert_eq!(accepted, input_id(&mut watch_a, &id_a)?);
    ids.push(accepted);
  }
  success(&mut a, json!({"operation":"release_turn","turn_id":token}))?;
  for observer in [&mut watch_a, &mut watch_b] {
    assert_eq!(observer.receive()?, free_event());
  }
  let denied = denied_action(&mut a, press("g", &token), "stale_turn")?;
  denials.push((action_id(&denied), "stale_turn", id_a.clone()));

  // Watch the host expire a turn without sending a command to advance its clock.
  let expired = acquire(&mut b, 10)?;
  for observer in [&mut watch_a, &mut watch_b] {
    held_event(observer, &id_b)?;
    assert_eq!(observer.receive()?, free_event());
  }
  let denied = denied_action(&mut b, press("h", &expired), "stale_turn")?;
  denials.push((action_id(&denied), "stale_turn", id_b.clone()));

  let disconnected = acquire(&mut a, 30_000)?;
  for observer in [&mut watch_a, &mut watch_b] {
    held_event(observer, &id_a)?;
  }
  a.close()?;
  assert_eq!(watch_b.receive()?, free_event());
  let end = [watch_a.receive()?, watch_a.receive()?];
  assert!(end.contains(&free_event()));
  assert!(end.contains(&json!({"type":"left","participant_id":id_a})));

  // A reconnect with the same label has fresh authority and cannot reuse the old token.
  let mut reopened = open(&callers[0], &win_a, "session", "writer")?;
  let denied = denied_action(&mut reopened, press("i", &disconnected), "stale_turn")?;
  let joined = watch_a.receive()?;
  assert_eq!(joined["type"], "joined");
  let new_id = &joined["participant"]["participant_id"];
  assert_ne!(new_id, &id_a);
  denials.push((action_id(&denied), "stale_turn", new_id.clone()));
  let accepted = accepted_action(
    &mut reopened,
    json!({"operation":"press_keys","keys":["c"]}),
  )?;
  assert_eq!(accepted, input_id(&mut watch_a, new_id)?);
  ids.push(accepted);
  for client in [reopened, b, legacy, watch_a, watch_b] {
    client.close()?;
  }

  let rows = activity_details(host)?;
  let requests: Vec<_> = rows
    .iter()
    .filter(|r| r["operation"] == "press_keys")
    .collect();
  assert_eq!(
    requests.len(),
    9,
    "three successful and six refused key requests: {rows:?}"
  );
  let distinct: HashSet<_> = requests
    .iter()
    .map(|row| row["action_id"].as_str().unwrap())
    .collect();
  assert_eq!(distinct.len(), requests.len());
  let account = format!("uid:{}", rustix::process::geteuid().as_raw());
  assert!(requests.iter().all(|row| row["account_id"] == account));
  for (id, code, participant) in denials {
    let row = requests
      .iter()
      .find(|row| row["action_id"] == id)
      .expect("SDK refusal in host audit");
    assert_eq!(row["status"], "failed");
    assert_eq!(row["refusal_code"], code);
    assert_eq!(row["session_id"], participant);
    if code == "busy" {
      assert_eq!(row["owner_id"], id_a);
    }
  }
  let legacy = requests
    .iter()
    .find(|row| row["agent_id"] == "legacy")
    .unwrap();
  assert_eq!(legacy["status"], "failed");
  assert_eq!(legacy["refusal_code"], "busy");
  assert_eq!(legacy["owner_id"], id_a);
  let successes: Vec<_> = requests
    .into_iter()
    .filter(|row| row["status"] == "succeeded")
    .collect();
  assert_eq!(successes.len(), ids.len());
  for (row, id) in successes.iter().rev().zip(ids) {
    assert_eq!(row["action_id"], id);
    assert_eq!(row["window_id"], win_a);
    assert!(row["refusal_code"].is_null());
  }
  assert!(successes[0]["turn_ref"].is_null());
  assert_eq!(successes[0]["session_id"], *new_id);
  assert_eq!(successes[1]["session_id"], id_a);
  let reference = successes[1]["turn_ref"].as_str().unwrap();
  Uuid::parse_str(reference)?;
  assert_ne!(reference, token);
  assert_eq!(successes[2]["turn_ref"], reference);
  for entry in fs::read_dir(host.join("ctream/activity"))? {
    let bytes = fs::read(entry?.path())?;
    for secret in [&token, &expired, &disconnected] {
      assert!(
        !bytes.windows(secret.len()).any(|s| s == secret.as_bytes()),
        "journal persisted a private turn ID"
      );
    }
  }
  for caller in &callers {
    assert!(fs::read_dir(caller)?.next().is_none());
  }

  let mut text = String::new();
  wait_for("physical keys from admitted turns", || {
    while let Some(event) = conn.poll_for_event()? {
      if let Event::KeyPress(key) = event {
        assert_eq!(key.event, xid, "input reached the competing window");
        let sym = conn.get_keyboard_mapping(key.detail, 1)?.reply()?.keysyms[0];
        text.push(char::from_u32(sym).unwrap());
      }
    }
    Ok((text.len() >= 3).then_some(()))
  })?;
  assert_eq!(text, "abc");
  Ok(())
}

#[test]
#[ignore = "requires the disposable X11 session and sibling CLI in docker/ctream"]
fn online_upgrade_preserves_native_view_and_sdk_session_through_two_generations()
-> Result<(), Box<dyn Error>> {
  use ctream_proto::live::LiveEvent;
  use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConfigureWindowAux};

  let fixture = HostFixture::new("swap")?;
  let caller = fixture.caller("sdk")?;
  let display = std::env::var("CTREAM_TEST_X11")?;
  let target = Target::parse(&display)?;
  let (conn, screen) = x11rb::connect(Some(&display))?;
  let (xid, window) = fixture_window(
    &conn,
    &conn.setup().roots[screen],
    target,
    30,
    b"Online upgrade",
  )?;
  conn
    .change_window_attributes(
      xid,
      &ChangeWindowAttributesAux::new()
        .event_mask(EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE),
    )?
    .check()?;
  let open = |op, label| {
    NamedClient::open(
      &caller,
      fixture.state(),
      &display,
      &window,
      op,
      Some(("1.0.7", label)),
    )
  };
  let mut sdk = open("session", "writer")?;
  let screenshot = |sdk: &mut NamedClient| success(sdk, json!({"operation":"screenshot"}));
  assert_eq!(screenshot(&mut sdk)?["width"], 220);
  let mut watch = open("watch", "watcher")?;
  let writer = writer_id(&watch.receive()?);
  assert_eq!(watch.receive()?, free_event());

  let client = fixture.client()?;
  let session = client.session(
    target,
    Some(ctream_proto::Window {
      id: window.clone(),
      ..Default::default()
    }),
  );
  let mut video = session.automation()?;
  assert_eq!(video.screenshot()?.size, [220, 100]);
  let (mut viewer, _connection) = session.viewer_observer("viewer")?.split();
  let secret = viewer.viewer_attachment()?;
  let mut input = session.viewer_control("viewer", None, Some(&secret))?;
  let Some(LiveEvent::Snapshot {
    self_id,
    participants,
  }) = viewer.next_event()?
  else {
    panic!("missing native roster")
  };
  assert_eq!(participants.len(), 3);
  assert_eq!(watch.receive()?["participant"]["participant_id"], self_id);
  assert!(matches!(
    viewer.next_event()?,
    Some(LiveEvent::Turn { holder: None, .. })
  ));

  let token = acquire(&mut sdk, 30_000)?;
  let original_turn = watch.receive()?;
  assert_eq!(original_turn["holder"]["participant_id"], writer);
  assert_eq!(
    serde_json::to_value(viewer.next_event()?.unwrap())?,
    original_turn
  );
  let generation = || -> Result<Value, Box<dyn Error>> {
    let out = fixture.command().args(["daemon", "status"]).output()?;
    assert!(
      out.status.success(),
      "{}",
      String::from_utf8_lossy(&out.stderr)
    );
    let status: Value = serde_json::from_slice(&out.stdout)?;
    let generation = status
      .get("generation")
      .filter(|id| id.is_string())
      .expect("online daemon generation");
    Ok(generation.clone())
  };
  let mut previous = generation()?;
  let operator_state = fixture.caller("operator")?;
  let context = || -> Result<Value, Box<dyn Error>> {
    let mut socket =
      std::os::unix::net::UnixStream::connect(fixture.state().join("daemon/v2.sock"))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.write_all(b"{\"maintenance\":{\"operation\":\"launch_context\"}}\n")?;
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
  };
  let desktop = context()?;
  let mut ids = Vec::new();
  for (current_width, width) in [(220, 225), (225, 230)] {
    // Dispatch on the established session while the candidate is starting.
    let upgrade = fixture
      .command()
      .args(["daemon", "upgrade", AGENT])
      .env("XDG_STATE_HOME", &operator_state)
      .env("XDG_RUNTIME_DIR", operator_state.join("other-runtime"))
      .env("XAUTHORITY", operator_state.join("other-authority"))
      .env("DISPLAY", ":9999")
      .env_remove("DBUS_SESSION_BUS_ADDRESS")
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()?;
    sdk.send(json!({"operation":"click","point":{"x":30,"y":20},"size":[current_width,100],"button":"left","turn_id":token}))?;
    let receipt = sdk.receive()?;
    assert_eq!(receipt["ok"], true, "{receipt}");
    let id = action_id(&receipt);
    let event = watch.receive()?;
    assert_eq!(event["action_id"], id);
    assert_eq!(event["participant_id"], writer);
    assert_eq!(viewer.next_event()?, Some(serde_json::from_value(event)?));
    ids.push(id);
    let upgraded = upgrade.wait_with_output()?;
    assert!(
      upgraded.status.success(),
      "{}",
      String::from_utf8_lossy(&upgraded.stderr)
    );
    let current = generation()?;
    assert_ne!(current, previous);
    assert_eq!(
      context()?,
      desktop,
      "operator's shell changed the live desktop"
    );
    previous = current;

    // Change the real source after takeover; the new dimensions prove these
    // are fresh frames on the same capture streams, rather than cached images.
    conn
      .configure_window(xid, &ConfigureWindowAux::new().width(width))?
      .check()?;
    wait_for("resumed SDK and native capture see resized source", || {
      let ready =
        screenshot(&mut sdk)?["width"] == width && video.screenshot()?.size[0] == width as usize;
      Ok(ready.then_some(()))
    })?;

    // A probe in a different room sees the desktop reservation without adding
    // join/leave noise to the session whose output order we are checking.
    let mut probe = NamedClient::open(
      &caller,
      fixture.state(),
      &display,
      "x11-1",
      "watch",
      Some(("1.0.7", "probe")),
    )?;
    assert_eq!(probe.receive()?["type"], "snapshot");
    assert_eq!(
      probe.receive()?,
      original_turn,
      "upgrade extended or replaced the turn"
    );
    probe.close()?;
  }
  success(
    &mut sdk,
    json!({"operation":"release_turn","turn_id":token}),
  )?;
  assert_eq!(watch.receive()?, free_event());
  assert!(matches!(
    viewer.next_event()?,
    Some(LiveEvent::Turn { holder: None, .. })
  ));
  input.toggle_turn()?;
  assert_eq!(watch.receive()?["holder"]["participant_id"], self_id);
  input.toggle_turn()?;

  conn.get_input_focus()?.reply()?;
  let mut physical = Vec::new();
  while let Some(event) = conn.poll_for_event()? {
    match event {
      Event::ButtonPress(ev) if ev.event == xid => physical.push((ev.detail, true)),
      Event::ButtonRelease(ev) if ev.event == xid => physical.push((ev.detail, false)),
      _ => {}
    }
  }
  assert_eq!(physical, [(1, true), (1, false), (1, true), (1, false)]);
  let rows = activity_details(fixture.state())?;
  assert!(
    fs::read_dir(operator_state)?.next().is_none(),
    "upgrade redirected the desktop's audit database"
  );
  assert_eq!(
    rows.len(),
    2,
    "each physical click must have exactly one audit row"
  );
  assert_eq!(rows[0]["turn_ref"], rows[1]["turn_ref"]);
  assert!(rows[0]["turn_ref"].is_string());
  for (row, id) in rows.iter().rev().zip(ids) {
    assert_eq!(row["action_id"], id);
    assert_eq!(row["session_id"], writer);
    assert_eq!(row["status"], "succeeded");
  }
  Ok(())
}
