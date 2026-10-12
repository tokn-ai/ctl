use avt::parser::{Parser, ParserState};
use avt::{Color, StateLimits, Vt, VtState};
use proptest::prelude::*;
use serde_json::{json, Value};

fn terminal(reflow_cursor_line: bool, scrollback_limit: Option<usize>) -> Vt {
    let mut builder = Vt::builder();
    builder.size(12, 5).reflow_cursor_line(reflow_cursor_line);
    if let Some(limit) = scrollback_limit {
        builder.scrollback_limit(limit);
    }
    builder.build()
}

fn checkpoint(term: &Vt) -> Vt {
    let bytes = serde_json::to_vec(&term.export_state()).unwrap();
    let state: VtState = serde_json::from_slice(&bytes).unwrap();
    let restored = Vt::from_state(state).unwrap();
    assert_equivalent(term, &restored);
    restored
}

fn assert_equivalent(expected: &Vt, actual: &Vt) {
    assert_eq!(expected.size(), actual.size());
    assert_eq!(expected.cursor(), actual.cursor());
    assert_eq!(expected.cursor_key_app_mode(), actual.cursor_key_app_mode());
    assert_eq!(expected.text(), actual.text());
    assert_eq!(
        expected.lines().cloned().collect::<Vec<_>>(),
        actual.lines().cloned().collect::<Vec<_>>()
    );
    assert_eq!(expected.export_state(), actual.export_state());
}

fn feed_chars(term: &mut Vt, input: &str) {
    input.chars().for_each(|ch| term.feed(ch));
}

fn feed_both(expected: &mut Vt, actual: &mut Vt, input: &str) {
    let expected_changes = {
        let changes = expected.feed_str(input);
        (changes.lines, changes.scrollback.collect::<Vec<_>>())
    };
    let actual_changes = {
        let changes = actual.feed_str(input);
        (changes.lines, changes.scrollback.collect::<Vec<_>>())
    };
    assert_eq!(expected_changes, actual_changes);
    assert_equivalent(expected, actual);
}

fn resize_both(expected: &mut Vt, actual: &mut Vt, cols: usize, rows: usize) {
    let expected_changes = {
        let changes = expected.resize(cols, rows);
        (changes.lines, changes.scrollback.collect::<Vec<_>>())
    };
    let actual_changes = {
        let changes = actual.resize(cols, rows);
        (changes.lines, changes.scrollback.collect::<Vec<_>>())
    };
    assert_eq!(expected_changes, actual_changes);
    assert_equivalent(expected, actual);
}

#[test]
fn every_escape_sequence_split_resumes_like_uninterrupted_input() {
    let streams = [
        "prefix\x1b[38;2;17;34;51m界\x1b[48;5;123m X\x1b[0m suffix",
        "\x1b[2;4r\x1b[?6h\x1b[2;3H\x1b[?1;7;25l\x1b[4;20h\x1b7x\x1b8z",
        "\x1b[3g\x1b[1;3H\x1bH\x1b[1;7H\x1bH\r\tX\x1b[Zy",
        "\x1b(0lqk\x1b)0\x0exqq\x0f\x1b(B ascii\x1b[s\x1b[4;5H\x1b[u!",
        "\x1b]0;ignored title\x07print\x1b]2;another title\x1b\\tail",
        "\x1bP1;2 $qignored payload\x1b\\text\x1bP:ignored\x1b\\done",
        "\x1b[1 3ignored\x1b[:ignored\x1b[31mred\x1b[0m",
        "\x1bXignored\x1b\\one\x1b^ignored\x1b\\two\x1b_ignored\x1b\\three",
        "\u{009b}38;2;17;34;51m界\u{009d}title\u{009c}\u{0090}1qignore\u{009c}!",
        "\x1b[?1049h界 alternate\x1b[3;2H\x1b7saved\x1b8!\x1b[?1049lprimary",
        "\x1b[999999999999999999999999999999999999;999999999999999999mX",
    ];
    for reflow in [false, true] {
        for stream in streams {
            let chars: Vec<_> = stream.chars().collect();
            for split in 0..=chars.len() {
                let mut uninterrupted = terminal(reflow, Some(8));
                chars[..split].iter().for_each(|ch| uninterrupted.feed(*ch));
                let mut restored = checkpoint(&uninterrupted);
                for ch in &chars[split..] {
                    uninterrupted.feed(*ch);
                    restored.feed(*ch);
                }
                feed_both(&mut uninterrupted, &mut restored, "\x18\r\ncontinued\t界");
                resize_both(&mut uninterrupted, &mut restored, 7, 3);
                feed_both(&mut uninterrupted, &mut restored, "\x1b8\x1b[?1049l\r\nend");
                resize_both(&mut uninterrupted, &mut restored, 18, 7);
            }
        }
    }
}

#[test]
fn standalone_parser_restores_every_state_and_pending_parameter_part() {
    let cases = [
        ("ground", "text"),
        ("escape", "\x1b"),
        ("escape_intermediate", "\x1b("),
        ("csi_entry", "\x1b["),
        ("csi_param", "\x1b[38;2;255;"),
        ("csi_intermediate", "\x1b[1 "),
        ("csi_ignore", "\x1b[:"),
        ("dcs_entry", "\x1bP"),
        ("dcs_param", "\x1bP1;2"),
        ("dcs_intermediate", "\x1bP1 "),
        ("dcs_passthrough", "\x1bP1qpayload"),
        ("dcs_ignore", "\x1bP:"),
        ("osc_string", "\x1b]title"),
        ("sos_pm_apc_string", "\x1b_string"),
    ];
    for (name, prefix) in cases {
        let mut uninterrupted = Parser::new();
        prefix.chars().for_each(|ch| {
            uninterrupted.feed(ch);
        });
        let value = serde_json::to_value(uninterrupted.export_state()).unwrap();
        assert_eq!(value["state"], name, "prefix must exercise {name}");
        let state: ParserState = serde_json::from_value(value).unwrap();
        let mut restored = Parser::from_state(state).unwrap();
        for ch in "0;128m\x07\x1b\\\x18\x1b[38:2::17:34:51m界\x1b(Bx\x1b[2;3H".chars() {
            assert_eq!(uninterrupted.feed(ch), restored.feed(ch), "state {name}");
            assert_eq!(uninterrupted.export_state(), restored.export_state());
        }
    }
}

#[test]
fn both_buffers_history_and_saved_context_survive_inactive_buffer_round_trips() {
    for active_alternate in [false, true] {
        let mut uninterrupted = terminal(false, Some(20));
        uninterrupted.feed_str(
            "history 界\r\nsecond\r\nthird\r\nfourth\r\nfifth\r\nprimary\
       \x1b[38;2;17;34;51m\x1b[3;5H\x1b7\x1b[?1049h\
       alternate 界\x1b[44m\x1b[2;3H\x1b7\x1b[4;8Hlast",
        );
        if !active_alternate {
            uninterrupted.feed_str("\x1b[?1049l");
        }
        let mut restored = checkpoint(&uninterrupted);
        feed_both(&mut uninterrupted, &mut restored, "\x1b8X\x1b[?1049l\x1b8Y");
        feed_both(&mut uninterrupted, &mut restored, "\x1b[?1047h\x1b8Z");
        resize_both(&mut uninterrupted, &mut restored, 7, 3);
        resize_both(&mut uninterrupted, &mut restored, 20, 8);
        feed_both(
            &mut uninterrupted,
            &mut restored,
            "\x1b[?1047l\x1b8done\r\n",
        );
    }
}

#[test]
fn cursor_modes_tabs_charsets_repeat_and_saved_pen_affect_future_output_identically() {
    let mut uninterrupted = terminal(true, Some(8));
    uninterrupted.feed_str(
        "\x1b[3g\x1b[1;3H\x1bH\x1b[1;8H\x1bH\
     \x1b[2;4r\x1b[?6h\x1b[?1h\x1b[?25l\x1b[?7l\x1b[4;20h\
     \x1b(0\x1b)0\x0e\x1b[1;32;45m\x1b[2;4Hq\x1b7\
     \x1b[0m\x1b[?6l\x1b[?7h\x1b[4l\x1b[1;1H",
    );
    assert!(uninterrupted.cursor_key_app_mode());
    assert!(!uninterrupted.cursor().visible);
    let mut restored = checkpoint(&uninterrupted);
    feed_both(
        &mut uninterrupted,
        &mut restored,
        "\x1b8\x1b[3b\tq\nq\x0flqk",
    );
    assert!(restored.lines().flat_map(|line| line.cells()).any(|cell| {
        cell.pen().foreground() == Some(Color::Indexed(2))
            && cell.pen().background() == Some(Color::Indexed(5))
    }));
    feed_both(
        &mut uninterrupted,
        &mut restored,
        "\x1b[?6l\r\tT\x1b[ZB\x1b(Btext",
    );
}

#[test]
fn pending_wrap_dirty_rows_and_history_collection_are_preserved() {
    let mut uninterrupted = terminal(false, Some(0));
    uninterrupted.feed_str("");
    feed_chars(&mut uninterrupted, "\x1b[5;1Habcdefghijkl");
    assert_eq!(uninterrupted.cursor().col, 12);
    let mut restored = checkpoint(&uninterrupted);
    feed_both(&mut uninterrupted, &mut restored, "");
    resize_both(&mut uninterrupted, &mut restored, 12, 7);
    feed_both(&mut uninterrupted, &mut restored, "X\r\n\r\n\r\n\r\nlast");
    assert_eq!(restored.lines().count(), restored.size().1);
}

#[test]
fn queued_scrollback_collection_survives_a_checkpoint_between_character_feeds() {
    let mut uninterrupted = terminal(false, Some(0));
    uninterrupted.feed_str("");
    feed_chars(
        &mut uninterrupted,
        "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven",
    );
    assert!(uninterrupted.lines().count() > uninterrupted.size().1);
    let mut restored = checkpoint(&uninterrupted);
    feed_both(&mut uninterrupted, &mut restored, "");
    assert_eq!(restored.lines().count(), restored.size().1);
    feed_both(&mut uninterrupted, &mut restored, "\r\neight");
}

#[test]
fn inactive_buffer_and_saved_cursor_keep_their_previous_size_until_switching_back() {
    let mut uninterrupted = terminal(false, Some(20));
    uninterrupted.feed_str("primary\x1b[5;12H\x1b[32m\x1b7\x1b[?1049halt\x1b[4;9H\x1b7");
    uninterrupted.resize(6, 2);
    let state = serde_json::to_value(uninterrupted.export_state()).unwrap();
    assert_eq!(state["terminal"]["cols"], 6);
    assert_eq!(state["terminal"]["other_buffer"]["cols"], 12);
    assert_eq!(state["terminal"]["alternate_saved_ctx"]["cursor_row"], 4);
    let mut restored = checkpoint(&uninterrupted);
    feed_both(&mut uninterrupted, &mut restored, "\x1b[?1049lX\x1b8Y");
    resize_both(&mut uninterrupted, &mut restored, 18, 6);
    feed_both(
        &mut uninterrupted,
        &mut restored,
        "\x1b[?1049hnew\x1b[?1049lZ",
    );
}

#[test]
fn boundary_tab_stop_and_pending_wrap_are_safe_to_restore() {
    let mut original = terminal(false, Some(8));
    original.feed_str("abcdefghijkl");
    assert_eq!(original.cursor().col, 12);
    let mut value = serde_json::to_value(original.export_state()).unwrap();
    value["terminal"]["tabs"] = json!([12]);
    let mut restored = Vt::from_state(serde_json::from_value(value).unwrap()).unwrap();
    restored.feed_str("\r\tX");
    assert_eq!(restored.line(0).text(), "abcdefghijkX");
    assert_eq!(restored.cursor().col, 12);
    restored.feed_str("Y");
    assert_eq!(restored.line(1).text().trim_end(), "Y");
}

#[test]
fn resize_policy_and_partial_csi_survive_repeated_serialized_checkpoints() {
    for reflow in [false, true] {
        let mut uninterrupted = terminal(reflow, Some(6));
        uninterrupted.feed_str("header 界界\r\n\x1b[34mprompt text\x1b[3");
        let mut restored = checkpoint(&uninterrupted);
        for (cols, rows) in [(5, 3), (12, 5), (8, 7), (16, 4), (6, 2)] {
            resize_both(&mut uninterrupted, &mut restored, cols, rows);
            restored = checkpoint(&restored);
        }
        feed_both(&mut uninterrupted, &mut restored, "1mX\r\x1b[Knext 界\r\n");
        feed_both(
            &mut uninterrupted,
            &mut restored,
            "\x1bcreset\r\nabcdefghijkl",
        );
        resize_both(&mut uninterrupted, &mut restored, 4, 4);
        resize_both(&mut uninterrupted, &mut restored, 14, 5);
    }
}

#[derive(Clone, Debug)]
enum Action {
    Feed(String),
    Resize(usize, usize),
}

fn actions() -> impl Strategy<Value = Vec<Action>> {
    let chunks = prop::sample::select(vec![
        "abc",
        "界",
        "\r\n",
        "\t",
        "\x1b",
        "[",
        "38;2;",
        "17;34;51m",
        "\x1b[0m",
        "\x1b[?1049h",
        "\x1b[?1049l",
        "\x1b[?1047h",
        "\x1b[?1047l",
        "\x1b7",
        "\x1b8",
        "\x1b[2;4r",
        "\x1b[?6h",
        "\x1b[?6l",
        "\x1b[?7h",
        "\x1b[?7l",
        "\x1b[4h",
        "\x1b[4l",
        "\x1b[3g",
        "\x1bH",
        "\x1b[2;3H",
        "\x1b[2K",
        "\x1b[J",
        "\x1b[3b",
        "\x1b(0",
        "\x1b(B",
        "\x0e",
        "\x0f",
        "\x1b]title",
        "\x07",
        "\x1bP1qpayload",
        "\x1b\\",
        "\x18",
    ]);
    let feed = prop::collection::vec(chunks, 0..5).prop_map(|chunks| Action::Feed(chunks.concat()));
    let resize = (2..20usize, 2..9usize).prop_map(|(cols, rows)| Action::Resize(cols, rows));
    prop::collection::vec(prop_oneof![4 => feed, 1 => resize], 1..25)
}

proptest! {
  #![proptest_config(ProptestConfig::with_cases(96))]

  #[test]
  fn arbitrary_feed_and_resize_continuations_match_uninterrupted_execution(
    actions in actions(),
    reflow in any::<bool>(),
    history in prop::option::of(0..20usize),
  ) {
    let mut uninterrupted = terminal(reflow, history);
    let mut restored = checkpoint(&uninterrupted);
    for action in actions {
      match action {
        Action::Feed(input) => {
          feed_chars(&mut uninterrupted, &input);
          feed_chars(&mut restored, &input);
          assert_equivalent(&uninterrupted, &restored);
        }
        Action::Resize(cols, rows) => {
          resize_both(&mut uninterrupted, &mut restored, cols, rows);
        }
      }
      restored = checkpoint(&restored);
    }
    feed_both(&mut uninterrupted, &mut restored, "\x18\x1b[?1049l\x1b8\r\nfinal界");
    resize_both(&mut uninterrupted, &mut restored, 11, 4);
  }
}

fn reject_state(value: Value) {
    let result = std::panic::catch_unwind(|| {
        match serde_json::from_value::<VtState>(value) {
            Ok(state) => Vt::from_state(state).map(|_| ()),
            Err(_) => return true,
        }
        .is_err()
    });
    assert!(result.is_ok(), "malformed state must not panic");
    assert!(result.unwrap(), "malformed state must be rejected");
}

#[test]
fn invalid_schema_geometry_parser_and_buffer_invariants_are_rejected_without_panics() {
    let pristine = serde_json::to_value(terminal(false, Some(8)).export_state()).unwrap();
    let mutations: &[(&str, Value)] = &[
        ("/version", json!(0)),
        ("/version", json!(999)),
        ("/parser/version", json!(999)),
        ("/parser/state", json!("not_a_parser_state")),
        ("/parser/cur_param", json!(32)),
        ("/parser/params/0/cur_part", json!(6)),
        ("/parser/intermediate", json!("x")),
        ("/parser/params", json!([])),
        ("/parser/params/0/parts", json!([])),
        ("/terminal/cols", json!(0)),
        ("/terminal/rows", json!(0)),
        ("/terminal/cols", json!(usize::MAX)),
        ("/terminal/rows", json!(usize::MAX)),
        ("/terminal/cursor/row", json!(5)),
        ("/terminal/cursor/col", json!(13)),
        ("/terminal/active_charset", json!(2)),
        ("/terminal/top_margin", json!(5)),
        ("/terminal/bottom_margin", json!(5)),
        ("/terminal/buffer/cols", json!(11)),
        ("/terminal/buffer/rows", json!(4)),
        ("/terminal/other_buffer/cols", json!(11)),
        ("/terminal/other_buffer/rows", json!(6)),
        ("/terminal/buffer/scrollback_limit", json!(0)),
        ("/terminal/other_buffer/scrollback_limit", json!(8)),
        ("/terminal/dirty_lines", json!([])),
        ("/terminal/tabs", json!([8, 8])),
        ("/terminal/tabs", json!([8, 2])),
        ("/terminal/tabs", json!([13])),
        ("/terminal/saved_ctx/cursor_row", json!(5)),
        ("/terminal/alternate_saved_ctx/cursor_col", json!(12)),
        ("/terminal/pen/attrs", json!(32)),
        ("/terminal/buffer/lines", json!([])),
        ("/terminal/other_buffer/lines", json!([])),
        ("/terminal/buffer/lines/0/cells", json!([])),
        ("/terminal/other_buffer/lines/0/cells", json!([])),
        (
            "/terminal/buffer/lines/0/cells/0/occupancy",
            json!("wide_tail"),
        ),
        (
            "/terminal/buffer/lines/0/cells/0/occupancy",
            json!("wide_head"),
        ),
        (
            "/terminal/other_buffer/lines/0/cells/0/occupancy",
            json!("wide_tail"),
        ),
        ("/terminal/buffer/lines/0/cells/0/ch", json!("界")),
        ("/terminal/buffer/lines/0/cells/0/pen/attrs", json!(32)),
        ("/terminal/buffer/lines/4/wrapped", json!(true)),
    ];
    for (path, invalid) in mutations {
        let mut value = pristine.clone();
        *value
            .pointer_mut(path)
            .unwrap_or_else(|| panic!("schema missing {path}")) = invalid.clone();
        reject_state(value);
    }
    let mut inverted_margins = pristine;
    inverted_margins["terminal"]["top_margin"] = json!(3);
    inverted_margins["terminal"]["bottom_margin"] = json!(1);
    reject_state(inverted_margins);
}

#[test]
fn missing_or_unknown_state_fields_are_rejected_instead_of_silently_defaulted() {
    let pristine = serde_json::to_value(terminal(false, Some(8)).export_state()).unwrap();
    let mut missing = pristine.clone();
    missing["terminal"]
        .as_object_mut()
        .unwrap()
        .remove("reflow_cursor_line");
    reject_state(missing);
    let mut unknown = pristine;
    unknown["terminal"]["future_mode"] = json!(true);
    reject_state(unknown);
}

#[test]
fn restore_limits_reject_valid_state_that_exceeds_the_callers_budget() {
    let state = terminal(false, Some(8)).export_state();
    for limits in [
        StateLimits {
            max_cols: 11,
            ..StateLimits::default()
        },
        StateLimits {
            max_rows: 4,
            ..StateLimits::default()
        },
        StateLimits {
            max_lines: 9,
            ..StateLimits::default()
        },
        StateLimits {
            max_cells: 119,
            ..StateLimits::default()
        },
        StateLimits {
            max_scrollback_limit: 7,
            ..StateLimits::default()
        },
    ] {
        assert!(Vt::from_state_with_limits(state.clone(), limits).is_err());
    }
    assert!(Vt::from_state_with_limits(state, StateLimits::default()).is_ok());
}
