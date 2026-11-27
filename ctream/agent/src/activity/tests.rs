use super::*;
use std::process::{Child, Command, Stdio};

impl Journal {
  fn session_id(&self) -> &str {
    &self.session_id
  }

  fn record(
    &mut self,
    agent_id: &str,
    target: Target,
    window_id: Option<&str>,
    operation: &'static str,
    apply: impl FnOnce() -> io::Result<()>,
  ) -> io::Result<()> {
    self.record_turn(agent_id, target, window_id, operation, None, |_| apply())
  }

  fn record_turn(
    &mut self,
    agent_id: &str,
    target: Target,
    window_id: Option<&str>,
    operation: &'static str,
    turn_ref: Option<&str>,
    apply: impl FnOnce(&str) -> io::Result<()>,
  ) -> io::Result<()> {
    let action_id = self.start(agent_id, target, window_id, operation, turn_ref)?;
    let result = apply(&action_id);
    self.finish(&action_id, Outcome::from_result(&result))?;
    result
  }
}

struct Fixture(PathBuf, Arc<Store>);

struct TestProcess(Child);

impl Drop for TestProcess {
  fn drop(&mut self) {
    if !matches!(self.0.try_wait(), Ok(Some(_))) {
      let _ = self.0.kill();
      let _ = self.0.wait();
    }
  }
}

impl Fixture {
  fn new() -> Self {
    let base = std::env::temp_dir().join(format!("ctream-activity-test-{}", Uuid::new_v4()));
    Self(base.clone(), Store::at(base))
  }

  fn journal(&self) -> Journal {
    self.1.for_session(&Uuid::new_v4().to_string()).unwrap()
  }

  fn child(&self, mode: &str) -> TestProcess {
    TestProcess(
      Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "activity::tests::subprocess", "--nocapture"])
        .env("CTREAM_ACTIVITY_TEST_BASE", &self.0)
        .env("CTREAM_ACTIVITY_TEST_MODE", mode)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap(),
    )
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    drop(self.1.writer.lock().unwrap().take());
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn subprocess() {
  let Some(base) = std::env::var_os("CTREAM_ACTIVITY_TEST_BASE") else {
    return;
  };
  let base = PathBuf::from(base);
  let mut journal = Store::at(base.clone())
    .for_session(&Uuid::new_v4().to_string())
    .unwrap();
  match std::env::var("CTREAM_ACTIVITY_TEST_MODE").as_deref() {
    Ok("crash") => {
      journal
        .record("worker", Target::X11(0), None, "click", || {
          std::process::exit(42)
        })
        .unwrap();
    }
    Ok("contend") => {
      journal
        .record("worker", Target::X11(0), None, "click", || {
          let rows = recent(&base, 2)?;
          assert_eq!(rows[0].status, "unknown");
          assert_eq!(rows[1].status, "unknown");
          Ok(())
        })
        .unwrap();
    }
    mode => panic!("unexpected test mode {mode:?}"),
  }
}

#[test]
fn independent_sessions_can_commit_while_a_driver_waits() {
  let fixture = Fixture::new();
  let mut journal = fixture.journal();
  let session_id = journal.session_id().to_owned();
  journal
    .record(
      "first",
      Target::WindowsDesktop,
      Some("window-1"),
      "drag",
      || {
        let rows = recent(&fixture.0, 2)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "unknown");
        let mut child = fixture.child("contend");
        assert!(child.0.wait()?.success());
        Ok(())
      },
    )
    .unwrap();
  let rows = recent(&fixture.0, 2).unwrap();
  assert_eq!(
    (rows[0].agent_id.as_str(), rows[0].status.as_str()),
    ("worker", "succeeded")
  );
  assert_eq!(
    (rows[1].agent_id.as_str(), rows[1].target.as_str()),
    ("first", "windows")
  );
  assert_eq!(rows[1].window_id.as_deref(), Some("window-1"));
  assert_eq!(rows[1].session_id, session_id);
  assert_ne!(rows[1].session_id, rows[0].session_id);
  assert_ne!(rows[1].action_id, rows[0].action_id);
}

#[test]
fn refusal_is_durable_while_another_request_is_in_flight() {
  let fixture = Fixture::new();
  let mut holder = fixture.journal();
  let mut contender = fixture.journal();
  let turn_ref = Uuid::new_v4().to_string();
  let original = holder
    .start("editor", Target::X11(1), None, "drag", Some(&turn_ref))
    .unwrap();
  let refused = contender
    .refuse(
      "inspector",
      Target::X11(1),
      Some("w-2"),
      "click",
      Some(&turn_ref),
      &TurnError {
        code: "busy".into(),
        owner_id: Some(holder.session_id().to_owned()),
      },
    )
    .unwrap();

  let rows = recent_with_context(&fixture.0, 2, true).unwrap();
  assert_eq!(rows[1].action_id, original);
  assert_eq!(rows[1].status, "unknown");
  let decision = &rows[0];
  assert_eq!(decision.status, "failed");
  assert_eq!(decision.refusal_code.as_deref(), Some("busy"));
  assert_eq!(decision.owner_id.as_deref(), Some(holder.session_id()));
  assert_eq!(decision.session_id, contender.session_id());
  assert_eq!(decision.action_id, refused);
  assert_eq!(decision.turn_ref.as_deref(), Some(turn_ref.as_str()));
  assert_ne!(original, refused);
  assert!(contender.finish(&original, Outcome::Succeeded).is_err());
  assert!(contender.finish(&refused, Outcome::Succeeded).is_err());
  assert!(holder.finish(&original, Outcome::Succeeded).is_ok());
  assert!(holder.finish(&original, Outcome::Failed).is_err());
  let legacy = serde_json::to_value(recent(&fixture.0, 2).unwrap()).unwrap();
  assert_eq!(legacy[0]["status"], "failed");
  assert!(legacy[0].get("refusal_code").is_none());
  assert!(legacy[0].get("owner_id").is_none());
  assert_eq!(legacy[1]["status"], "succeeded");
}

#[test]
fn refusal_metadata_rejects_arbitrary_messages_and_owner_ids() {
  let fixture = Fixture::new();
  let journal = fixture.journal();
  for (code, owner) in [
    ("sensitive error message", None),
    ("busy", Some("sensitive content")),
  ] {
    let error = journal
      .refuse(
        "inspector",
        Target::X11(1),
        None,
        "click",
        None,
        &TurnError {
          code: code.to_owned(),
          owner_id: owner.map(str::to_owned),
        },
      )
      .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
  }
  let action_id = journal
    .refuse(
      "inspector",
      Target::X11(1),
      None,
      "click",
      None,
      &TurnError {
        code: "stale_turn".into(),
        owner_id: None,
      },
    )
    .unwrap();
  let rows = recent_with_context(&fixture.0, 3, true).unwrap();
  assert_eq!(rows.len(), 1);
  assert_eq!(rows[0].action_id, action_id);
  assert_eq!(rows[0].refusal_code.as_deref(), Some("stale_turn"));
  assert!(rows[0].owner_id.is_none());
}

#[test]
fn interrupted_action_stays_unknown_and_driver_failure_stays_redacted() {
  let fixture = Fixture::new();
  assert_eq!(fixture.child("crash").0.wait().unwrap().code(), Some(42));
  let mut journal = fixture.journal();
  let error = journal
    .record(
      "second",
      Target::Wayland(1),
      Some("w-2"),
      "set_text",
      || Err(io::Error::other("sensitive detail")),
    )
    .unwrap_err();
  assert_eq!(error.to_string(), "sensitive detail");
  let rows = recent(&fixture.0, 2).unwrap();
  assert_eq!(rows[0].status, "failed");
  assert_eq!(rows[1].status, "unknown");
  assert!(
    !serde_json::to_string(&rows)
      .unwrap()
      .contains("sensitive detail")
  );
  assert_eq!(recent(&fixture.0, 1).unwrap().len(), 1);
  assert!(recent(&fixture.0, MAX_RECENT + 1).is_err());
  let other = Fixture::new();
  assert!(recent(&other.0, 1).unwrap().is_empty());
  assert!(!other.0.exists());
}

#[cfg(unix)]
#[test]
fn sdk_records_lost_driver_outcomes_only_after_dispatch() {
  use ctream_client::{
    Client, sdk,
    transport::{Channel, Local, Transport},
  };

  // Real subprocess pipes exercise signal exit, typed agent exit, clean EOF
  // and corrupt framing through the same SDK interpreter used by the host.
  struct Helper(&'static str);
  impl Transport for Helper {
    type Reader = <Local as Transport>::Reader;
    type Writer = <Local as Transport>::Writer;
    fn open(&self, args: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
      let script = match &*args[1] {
        "input-capabilities" => r#"printf '{"sync":true}'"#.to_owned(),
        "input-sync" => format!(
          r#"
          fail() {{ echo 'unexpected fixture request' >&2; exit 65; }}
          printf '64 64\n'
          read -r command && test "$command" = sync || fail
          {}
          printf 'ok\n'
          read -r down && read -r up && read -r sync || fail
          test "$down" = 'key a 1' && test "$up" = 'key a 0' && test "$sync" = sync || fail
          {}
        "#,
          if self.0 == "preflight" { "exit 0" } else { ":" },
          self.0
        ),
        "action" => format!(
          r#"
          payload=$(cat)
          case "$payload" in *sentinel*) ;; *) echo 'unexpected fixture request' >&2; exit 65;; esac
          {}
        "#,
          self.0
        ),
        other => panic!("unexpected helper operation {other}"),
      };
      Local {
        agent: "/bin/sh".into(),
      }
      .open_delegated(&["-c".into(), script], "fixture")
    }
  }

  let fixture = Fixture::new();
  let mut journal = fixture.journal();
  for (target, script, expected) in [
    (Target::X11(1), "preflight", "failed"),
    (Target::X11(1), "kill -TERM $$", "unknown"),
    (Target::X11(1), "printf 'bad'", "unknown"),
    (
      Target::X11(1),
      "printf 'native error\\n' >&2; exit 1",
      "unknown",
    ),
    (
      Target::X11(1),
      "printf 'native refusal\\n' >&2; exit 65",
      "failed",
    ),
    (Target::Wayland(1), "kill -TERM $$", "unknown"),
    (Target::Wayland(1), "printf 'truncated{'", "unknown"),
    (
      Target::Wayland(1),
      r#"printf '{"applied":false,"error":"declined"}'"#,
      "failed",
    ),
  ] {
    let (window, request) = match target {
      Target::X11(_) => (None, r#"{"operation":"press_keys","keys":["a"]}"#),
      _ => (
        Some("window-1"),
        r#"{"operation":"set_text","path":"/element","text":"sentinel"}"#,
      ),
    };
    let mut response = Vec::new();
    sdk::run_attributed_session(
      &Client::new(Helper(script)),
      target,
      window.map(str::to_owned),
      format!("{request}\n").as_bytes(),
      &mut response,
      |operation, apply| journal.record("test", target, window, operation, apply),
    )
    .unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(reply["ok"], false, "{script}: {reply}");
    assert!(
      !reply["error"]
        .as_str()
        .unwrap()
        .contains("unexpected fixture request")
    );
    let rows = recent(&fixture.0, 1).unwrap();
    assert_eq!(rows[0].status, expected, "{script}: {reply}");
    assert!(!serde_json::to_string(&rows).unwrap().contains("sentinel"));
  }
}

#[test]
fn detailed_history_reads_legacy_rows_and_keeps_unconfirmed_delivery_unknown() {
  let fixture = Fixture::new();
  let mut journal = fixture.journal();
  journal
    .record("original", Target::X11(1), None, "move", || Ok(()))
    .unwrap();
  let db = open_db(&prepare(&fixture.0, false).unwrap(), true).unwrap();
  db.execute_batch(
    "DROP TRIGGER prune_action_context; DROP TABLE action_context;
                    DROP TRIGGER prune_action_refusals; DROP TABLE action_refusals;",
  )
  .unwrap();
  let old = recent_with_context(&fixture.0, 2, true).unwrap();
  assert_eq!(old[0].account_id, None);
  // The reader is a legacy database with no metadata tables; a new writer
  // creates them when the daemon next opens a session.
  drop(journal);
  let mut journal = Store::at(fixture.0.clone())
    .for_session(&Uuid::new_v4().to_string())
    .unwrap();
  let group = Uuid::new_v4().to_string();
  let mut expected = String::new();
  journal
    .record_turn(
      "worker",
      Target::X11(1),
      None,
      "click",
      Some(&group),
      |id| {
        expected = id.to_owned();
        Err(ctream_driver::UnconfirmedDelivery::error(
          io::ErrorKind::UnexpectedEof.into(),
        ))
      },
    )
    .unwrap_err();
  let rows = recent_with_context(&fixture.0, 2, true).unwrap();
  assert_eq!(rows[0].status, "unknown");
  assert_eq!(rows[0].action_id, expected);
  assert_eq!(rows[0].turn_ref.as_deref(), Some(group.as_str()));
  assert_eq!(
    rows[0].account_id.as_deref(),
    Some(host_account_id().unwrap().as_str())
  );
  assert!(rows[1].turn_ref.is_none() && rows[1].account_id.is_none());
  let old_shape = serde_json::to_value(recent(&fixture.0, 1).unwrap()).unwrap();
  assert!(old_shape[0].get("account_id").is_none());
  assert!(old_shape[0].get("turn_ref").is_none());
}

#[test]
fn outcome_write_error_keeps_the_committed_intent_unknown() {
  let fixture = Fixture::new();
  let error = fixture
    .journal()
    .record("worker", Target::X11(0), None, "click", || {
      let db = open_db(&prepare(&fixture.0, false)?, true)?;
      db.execute_batch(
        "CREATE TRIGGER fail_outcome BEFORE UPDATE ON actions
      BEGIN SELECT RAISE(FAIL,'simulated write failure'); END",
      )
      .map_err(io::Error::other)
    })
    .unwrap_err();
  assert!(error.to_string().contains("may already have run"));
  assert_eq!(recent(&fixture.0, 1).unwrap()[0].status, "unknown");
}

#[test]
fn an_in_flight_request_aging_out_of_history_can_still_complete() {
  let fixture = Fixture::new();
  let mut journal = fixture.journal();
  let held = journal
    .start("tester", Target::X11(1), None, "drag", None)
    .unwrap();
  let dir = prepare(&fixture.0, false).unwrap();
  let db = open_db(&dir, true).unwrap();
  db.execute("WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM seq WHERE n < ?1)
    INSERT INTO actions SELECT 'seed-' || n,'worker','tester',1,':1',NULL,'move','succeeded' FROM seq",
    [RETAINED - 1]).unwrap();
  fixture
    .journal()
    .record("second", Target::X11(1), None, "move", || Ok(()))
    .unwrap();
  let count: i64 = db
    .query_row("SELECT count(*) FROM actions", [], |row| row.get(0))
    .unwrap();
  assert_eq!(count, RETAINED);
  let first: String = db
    .query_row(
      "SELECT action_id FROM actions ORDER BY rowid LIMIT 1",
      [],
      |row| row.get(0),
    )
    .unwrap();
  assert_eq!(first, "seed-1");
  journal.finish(&held, Outcome::Succeeded).unwrap();
}

#[cfg(unix)]
#[test]
fn unsafe_permissions_block_dispatch() {
  use std::os::unix::fs::PermissionsExt;
  let fixture = Fixture::new();
  let dir = prepare(&fixture.0, true).unwrap();
  fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
  let error = fixture
    .journal()
    .record("tester", Target::X11(0), None, "click", || {
      panic!("dispatched")
    })
    .unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn unrelated_database_blocks_dispatch_and_preserves_contents() {
  let fixture = Fixture::new();
  let db = open_db(&prepare(&fixture.0, true).unwrap(), true).unwrap();
  db.execute("CREATE TABLE existing(value TEXT)", []).unwrap();
  let error = fixture
    .journal()
    .record("tester", Target::X11(0), None, "click", || {
      panic!("dispatched")
    })
    .unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::InvalidData);
  assert_eq!(
    db.query_row("SELECT name FROM sqlite_schema", [], |row| row
      .get::<_, String>(0))
      .unwrap(),
    "existing"
  );
}
