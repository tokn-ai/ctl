use super::*;
use crate::mouse::MouseEncoding;
use serde_json::{Value, json};

fn feed(modes: &mut TerminalInputModes, text: &str) -> Vec<bool> {
  text.chars().map(|ch| modes.feed(ch)).collect()
}

fn serialized_state(modes: &TerminalInputModes) -> TerminalInputModesState {
  serde_json::from_slice(&serde_json::to_vec(&modes.export_state()).unwrap()).unwrap()
}

#[test]
fn serialized_prefixes_preserve_suffix_modes_and_query_results() {
  let oversized = format!("\x1b[?{}1002h\x1b[?4m", "0;".repeat(40));
  for text in [
    "\x1b[?1000;1005;1006;1015;1016;2004h\x1b[>4;2m\x1b[?4m",
    "\x1b[?1003;1006h\x1b[?1003;1006l\x1b[?2004h\x1b[?2004l",
    "\x1b[>4;1m\x1b[>4;2m\x1b[>4n\x1b[?4m\x1b[>4m",
    "\x1b[>1u\x1b[=3;1u\x1b[<u\x1b[?4m",
    "\x1b]mode [?2004h\x07\x1bPignored [?1003h\x1b\\",
    "\u{9d}title\u{9c}\u{9b}?1002;1006h\u{9b}>4;2m\u{9b}?4m",
    "\x1b[?1002\x18h\x1b[>4;2\x1am\x1b[?1002h\x1bc",
    "\x1b[?1002\x00;1006h\x1b[>4;\x1b[>4;2m",
    oversized.as_str(),
  ] {
    for boundary in text
      .char_indices()
      .map(|(index, _)| index)
      .chain([text.len()])
    {
      let mut original = TerminalInputModes::default();
      feed(&mut original, &text[..boundary]);
      let before = original.export_state();
      let state = serialized_state(&original);
      assert_eq!(
        original.export_state(),
        before,
        "export changed prefix state"
      );
      let mut restored = TerminalInputModes::from_state(state).unwrap();
      assert_eq!(restored, original, "prefix at {boundary} in {text:?}");
      assert_eq!(
        feed(&mut restored, &text[boundary..]),
        feed(&mut original, &text[boundary..]),
        "query result at {boundary} in {text:?}"
      );
      assert_eq!(restored, original, "suffix at {boundary} in {text:?}");
    }
  }
}

#[test]
fn restoration_preserves_inactive_encoding_bits_and_keyboard_level() {
  let mut original = TerminalInputModes::default();
  feed(
    &mut original,
    "\x1b[?1003;1005;1006;1015;1016;2004h\x1b[>4;2m",
  );
  let mut restored = TerminalInputModes::from_state(serialized_state(&original)).unwrap();
  assert_eq!(restored.modify_other_keys(), ModifyOtherKeys::Mode2);
  assert!(restored.bracketed_paste());
  for (reset, encoding) in [
    ("\x1b[?1016l", MouseEncoding::Sgr),
    ("\x1b[?1006l", MouseEncoding::Urxvt),
    ("\x1b[?1015l", MouseEncoding::Utf8),
    ("\x1b[?1005l", MouseEncoding::Legacy),
  ] {
    feed(&mut original, reset);
    feed(&mut restored, reset);
    assert_eq!(restored, original);
    assert_eq!(restored.mouse().encoding(), encoding);
  }
}

fn invalid_state(field: &str, value: Value) -> Result<TerminalInputModes, ObserverStateError> {
  let mut state = serde_json::to_value(TerminalInputModes::default().export_state()).unwrap();
  state[field] = value;
  TerminalInputModes::from_state(serde_json::from_value(state).unwrap())
}

#[test]
fn import_rejects_unknown_versions_encoding_bits_and_invalid_prefixes() {
  assert_eq!(
    invalid_state("version", json!(2)),
    Err(ObserverStateError::UnsupportedVersion {
      component: "input modes",
      version: 2
    })
  );
  assert!(invalid_state("mouse_encodings", json!(16)).is_err());
  for prefix in [
    "0".repeat(65),
    "?2004h".into(),
    "\x1b".into(),
    "\u{7f}".into(),
    "é".into(),
  ] {
    assert!(invalid_state("control", json!({ "csi": prefix })).is_err());
  }
  assert!(invalid_state("control", json!({ "csi": "0".repeat(64) })).is_ok());
}

#[test]
fn serialization_rejects_unknown_and_duplicate_fields_without_restoring() {
  let encoded = serde_json::to_string(&TerminalInputModes::default().export_state()).unwrap();
  let duplicate = encoded.replacen('{', "{\"version\":1,", 1);
  assert!(serde_json::from_str::<TerminalInputModesState>(&duplicate).is_err());
  let mut state: Value = serde_json::from_str(&encoded).unwrap();
  state["unknown"] = json!(true);
  assert!(serde_json::from_value::<TerminalInputModesState>(state).is_err());
  let mut state: Value = serde_json::from_str(&encoded).unwrap();
  state["control"] = json!({ "string": { "osc": true, "unknown": true } });
  assert!(serde_json::from_value::<TerminalInputModesState>(state).is_err());
}
