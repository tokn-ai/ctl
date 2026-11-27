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
