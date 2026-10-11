use super::*;
use serde_json::{Value, json};

fn serialized_state(tracker: &AlternateScreenTracker) -> AlternateScreenState {
  serde_json::from_slice(&serde_json::to_vec(&tracker.export_state()).unwrap()).unwrap()
}

#[test]
fn serialized_byte_prefixes_preserve_every_transition_and_partial_control() {
  let oversized = [b"\x1b[?".as_slice(), &[b'0'; 70], b"1049h\x1b[?1049h"].concat();
  for data in [
    b"\x1b[?47;1047;1049h\x1b[?1049l\x1b[?47l\x1b[?1047l".as_slice(),
    b"prompt\x1b\x1b[?1049h\x1b[?10\x1b[?47h\x1b[?1049;47l",
    b"\x1b[2J\x1b[1049h\x1b[?2004h\x1b[?1002;1006h",
    b"\x1b[?\xff\xc3\xa9\x00;1049h\x1b[?1049l",
    oversized.as_slice(),
  ] {
    for boundary in 0..=data.len() {
      let mut original = AlternateScreenTracker::default();
      original.observe(&data[..boundary]);
      let before = original.export_state();
      let state = serialized_state(&original);
      assert_eq!(
        original.export_state(),
        before,
        "export changed prefix state"
      );
      let mut restored = AlternateScreenTracker::from_state(state).unwrap();
      assert_eq!(restored, original, "prefix at {boundary} in {data:?}");
      for byte in &data[boundary..] {
        assert_eq!(restored.observe(&[*byte]), original.observe(&[*byte]));
        assert_eq!(restored, original, "suffix at {boundary} in {data:?}");
      }
      assert_eq!(restored.reset(), original.reset());
      assert_eq!(restored, original);
    }
  }
}

#[test]
fn private_modes_remain_independent_after_import() {
  let mut original = AlternateScreenTracker::default();
  assert_eq!(
    original.observe(b"\x1b[?47;1047;1049h"),
    Some(TuiHint::AlternateScreen)
  );
  let mut restored = AlternateScreenTracker::from_state(serialized_state(&original)).unwrap();
  assert_eq!(restored.observe(b"\x1b[?1049l"), None);
  assert_eq!(restored.observe(b"\x1b[?47l"), None);
  assert_eq!(restored.hint(), TuiHint::AlternateScreen);
  assert_eq!(restored.observe(b"\x1b[?1047l"), Some(TuiHint::Inline));
}

fn invalid_state(field: &str, value: Value) -> Result<AlternateScreenTracker, ObserverStateError> {
  let mut tracker = AlternateScreenTracker::default();
  tracker.observe(b"prompt");
  let mut state = serde_json::to_value(tracker.export_state()).unwrap();
  state[field] = value;
  AlternateScreenTracker::from_state(serde_json::from_value(state).unwrap())
}

#[test]
fn import_rejects_unknown_versions_bits_hints_and_invalid_prefixes() {
  assert_eq!(
    invalid_state("version", json!(2)),
    Err(ObserverStateError::UnsupportedVersion {
      component: "alternate screen",
      version: 2
    })
  );
  assert!(invalid_state("active_modes", json!(8)).is_err());
  assert!(invalid_state("active_modes", json!(1)).is_err());
  assert!(invalid_state("tui_hint", json!("alternate_screen")).is_err());
  for prefix in [vec![b'0'; 65], vec![b'h'], vec![0x1b]] {
    assert!(invalid_state("parser", json!({ "csi": prefix })).is_err());
  }
  assert!(invalid_state("parser", json!({ "csi": vec![b'0'; 64] })).is_ok());
  assert!(invalid_state("parser", json!({ "csi": [0xff, 0x00, b'?'] })).is_ok());
  let mut state = AlternateScreenTracker::default().export_state();
  state.parser = CsiObserverState::Escape;
  assert!(AlternateScreenTracker::from_state(state).is_err());
}

#[test]
fn serialization_rejects_unknown_and_duplicate_fields_without_restoring() {
  let encoded = serde_json::to_string(&AlternateScreenTracker::default().export_state()).unwrap();
  let duplicate = encoded.replacen('{', "{\"version\":1,", 1);
  assert!(serde_json::from_str::<AlternateScreenState>(&duplicate).is_err());
  let mut state: Value = serde_json::from_str(&encoded).unwrap();
  state["unknown"] = json!(true);
  assert!(serde_json::from_value::<AlternateScreenState>(state).is_err());
}
