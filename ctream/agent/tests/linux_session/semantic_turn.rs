use super::*;
use ctream_client::semantic_session::SemanticSession;
use ctream_proto::Action;

struct GtkWindow(Child);
impl Drop for GtkWindow {
  fn drop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}

#[test]
#[ignore = "requires the disposable Wayland compositor and accessibility bus in docker/ctream"]
fn viewer_turn_applies_atspi_actions_and_reports_audit_ids() -> Result<(), Box<dyn Error>> {
  let fixture = HostFixture::new("semantic")?;
  let caller = fixture.caller("viewer")?;
  let host = fixture.state();
  let client = fixture.client()?;
  let display = std::env::var("WAYLAND_DISPLAY")?;
  let target = Target::parse(&display)?;
  let before: HashSet<_> = client.windows(target)?.into_iter().map(|w| w.id).collect();
  // Use a second fixture process so invoking its button leaves the shared
  // window available to the existing AT-SPI integration tests.
  let _gtk = GtkWindow(
    Command::new("python3")
      .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/ctream/window.py"))
      .spawn()?,
  );
  let window = wait_for("second GTK window", || {
    Ok(
      client
        .windows(target)?
        .into_iter()
        .find(|w| w.title == "Ctream Wayland fixture" && !before.contains(&w.id)),
    )
  })?;
  let mut observer = NamedClient::open(
    &caller,
    host,
    &display,
    &window.id,
    "watch",
    Some(("1.0.6", "observer")),
  )?;
  let mut next = |phase: &str| {
    observer
      .receive()
      .map_err(|e| io::Error::new(e.kind(), format!("waiting for {phase}: {e}")))
  };
  let selected = client.session(target, Some(window));
  let mut controller = SemanticSession::new(selected.clone());
  // Wait for the watcher to join before opening the participant whose join
  // event we assert below. Starting the watch process alone is asynchronous.
  assert_eq!(next("observer snapshot")?["type"], "snapshot");
  assert_eq!(next("initial turn status")?["holder"], Value::Null);
  let (mut viewer_events, _viewer_watch) = selected.viewer_observer("semantic-viewer")?.split();
  let attachment = viewer_events.viewer_attachment()?;
  let Some(ctream_proto::live::LiveEvent::Snapshot { self_id, .. }) = viewer_events.next_event()?
  else {
    panic!("semantic viewer omitted its observer");
  };
  controller.connect_viewer("semantic-viewer", Some(&attachment))?;
  let joined = next("semantic viewer to join")?;
  assert_eq!(joined["participant"]["agent_id"], "semantic-viewer");
  let id = &joined["participant"]["participant_id"];
  assert_eq!(*id, self_id);

  let elements = wait_for("GTK accessibility controls", || {
    let items = selected.elements()?;
    Ok(
      items
        .iter()
        .any(|e| e.name == "Fixture button")
        .then_some(items),
    )
  })?;
  let entry = elements.iter().find(|e| e.editable).unwrap();
  let button = elements
    .iter()
    .find(|e| e.name == "Fixture button")
    .unwrap();
  controller.acquire(30_000)?;
  assert_eq!(
    next("semantic viewer to acquire")?["holder"]["participant_id"],
    *id
  );

  let competitor = selected.action(Action::SetText {
    path: entry.path.clone(),
    text: "denied".into(),
  });
  assert!(match competitor {
    Ok(outcome) => !outcome.applied,
    Err(error) => error.to_string().contains("busy"),
  });
  assert!(
    selected.elements()?.iter().any(|e| e.editable),
    "inspection was blocked by the turn"
  );
  assert!(
    controller
      .apply(Action::SetText {
        path: entry.path.clone(),
        text: "viewer draft".into()
      })?
      .applied
  );
  assert!(
    controller
      .apply(Action::Invoke {
        path: button.path.clone(),
        index: 0,
        name: button.actions[0].clone()
      })?
      .applied
  );
  assert!(
    selected.elements()?.iter().any(|e| e.name == "Clicked"),
    "AT-SPI invocation did not reach the GTK fixture"
  );
  let mut ids = Vec::new();
  for operation in ["set_text", "invoke"] {
    let event = loop {
      let event = next("an audited semantic action")?;
      if event["type"] == "action" {
        break event;
      }
      // The independent retained control process may announce a short-lived participant.
      assert!(
        matches!(event["type"].as_str(), Some("joined" | "left")),
        "{event}"
      );
    };
    assert_eq!(event["participant_id"], *id);
    assert_eq!(event["action"], json!({"operation":operation}));
    ids.push(event["action_id"].as_str().unwrap().to_owned());
  }
  controller.release()?;
  assert_eq!(
    next("semantic viewer to release")?,
    json!({"type":"turn","holder":null,"expires_at_ms":null})
  );
  let rows = activity_details(host)?;
  assert_eq!(rows.len(), 3);
  let rejected = &rows[2];
  assert_eq!(rejected["operation"], "set_text");
  assert_eq!(rejected["status"], "failed");
  assert_eq!(rejected["refusal_code"], "busy");
  assert_eq!(rejected["owner_id"], *id);
  assert_ne!(rejected["session_id"], *id);
  for (row, action_id) in rows[..2].iter().rev().zip(ids) {
    assert_eq!(row["action_id"], action_id);
    assert_eq!(row["session_id"], *id);
    assert_eq!(row["agent_id"], "semantic-viewer");
    assert_eq!(row["status"], "succeeded");
  }
  observer.close()?;
  Ok(())
}
