fn vt(columns: usize, rows: usize, preserve: bool) -> avt::Vt {
    avt::Vt::builder()
        .size(columns, rows)
        .scrollback_limit(10000)
        .reflow_cursor_line(!preserve)
        .build()
}
fn view(vt: &avt::Vt) -> Vec<String> {
    vt.view().map(|l| l.text().trim_end().to_owned()).collect()
}
fn prompt_case(preserve: bool) -> avt::Vt {
    let mut term = vt(60, 8, preserve);
    let prompt = "\x1b[34mclouds\x1b[39m@Clouds-MacbookM2 ~ % ";
    term.feed_str(&format!(
        "Restored session: Wed Oct 7 19:23:45 CST 2026\r\n{prompt}"
    ));
    term.resize(22, 8);
    term.feed_str(&format!("\r\r\x1b[0m\x1b[27m\x1b[24m\x1b[J{prompt}"));
    term.resize(60, 8);
    term.feed_str(&format!("\r\r\x1b[A\x1b[0m\x1b[27m\x1b[24m\x1b[J{prompt}"));
    term
}
#[test]
fn actual_zsh_resize_redraw_keeps_header_and_prompt() {
    let term = prompt_case(true);
    assert_eq!(
        view(&term)[..2],
        [
            "Restored session: Wed Oct 7 19:23:45 CST 2026",
            "clouds@Clouds-MacbookM2 ~ %"
        ]
    );
    assert_eq!((term.cursor().col, term.cursor().row), (28, 1));
    assert_eq!(term.lines().count(), 8);
}
#[test]
fn default_resize_retains_upstream_behaviour() {
    let mut term = vt(60, 8, false);
    term.feed_str("Restored session: Wed Oct 7 19:23:45 CST 2026\r\nclouds@Clouds-MacbookM2 ~ % ");
    term.resize(22, 8);
    assert_eq!(view(&term)[..2], ["clouds@Clouds-MacbookM", "2 ~ %"]);
    assert_eq!((term.cursor().col, term.cursor().row), (6, 1));
    assert_eq!(term.lines().count(), 11);
}
#[test]
fn same_width_resize_retains_pending_wrap() {
    for rows in [3, 5, 2, 2, 3] {
        let mut term = vt(6, 3, true);
        term.feed_str("abcdef");
        assert_eq!(term.cursor().col, 6);
        term.resize(6, rows);
        assert_eq!(term.cursor().col, 6);
        term.feed_str("g");
        assert_eq!(view(&term)[..2], ["abcdef", "g"]);
        assert_eq!((term.cursor().col, term.cursor().row), (1, 1));
    }
}
#[test]
fn cropped_wide_character_does_not_leave_an_orphan_head() {
    let mut term = vt(6, 3, true);
    term.feed_str("a界b");
    term.resize(2, 3);
    assert_eq!(view(&term)[0], "a");
    term.feed_str("x");
    assert_eq!(view(&term)[0], "ax");
    assert!(term.line(0).cells().iter().all(|c| c.width() != 2));
}
#[test]
fn bounded_history_retains_wide_text_above_cursor() {
    let mut term = vt(8, 3, true);
    term.feed_str("一二三四\r\nfive\r\nsix\r\nlive");
    term.resize(5, 3);
    term.resize(12, 3);
    assert!(
        term.text()[0].replace(" ", "") == "一二三四",
        "{:?}",
        term.text()
    );
    assert_eq!(view(&term).last().unwrap(), "live");
}
#[test]
fn alternate_resize_policy_remains_upstream() {
    let mut original = vt(10, 4, false);
    let mut interactive = vt(10, 4, true);
    let data = "\x1b[?1049habcdefghijklmno\x1b[3;1Htail";
    original.feed_str(data);
    interactive.feed_str(data);
    for (cols, rows) in [(6, 3), (20, 4), (3, 2), (15, 8)] {
        original.resize(cols, rows);
        interactive.resize(cols, rows);
        assert_eq!(view(&original), view(&interactive));
        assert_eq!(original.cursor(), interactive.cursor());
    }
}
#[test]
fn unused_padding_is_consumed_only_after_cursor() {
    let mut term = vt(60, 8, true);
    term.feed_str("Restored session: Wed Oct 7 19:23:45 CST 2026\r\nprompt");
    term.resize(22, 8);
    assert_eq!(term.lines().count(), 8);
    assert_eq!(
        view(&term)[..4],
        [
            "Restored session: Wed",
            "Oct 7 19:23:45 CST 202",
            "6",
            "prompt"
        ]
    );
    assert_eq!((term.cursor().col, term.cursor().row), (6, 3));
}

#[test]
fn populated_rows_below_cursor_keep_its_physical_row_visible() {
    let mut term = vt(6, 3, true);
    term.feed_str("header\r\nprompt\r\nabcdef\x1b[1;1H");
    term.resize(3, 3);
    assert_eq!(view(&term), ["hea", "pro", "mpt"]);
    assert_eq!((term.cursor().col, term.cursor().row), (0, 0));
    term.feed_str("X");
    assert_eq!(view(&term)[0], "Xea");
}

#[test]
fn blank_continuation_row_stays_in_the_protected_logical_line() {
    let mut term = vt(6, 3, true);
    term.feed_str("header\r\nabcdefg\r\x1b[6X\x1b[2;1H");
    term.resize(3, 3);
    assert_eq!(view(&term), ["der", "abc", ""]);
    assert_eq!((term.cursor().col, term.cursor().row), (0, 1));
    assert_eq!(wrapped_rows(&term), [true, false, true, false]);
}

fn wrapped_rows(term: &avt::Vt) -> Vec<bool> {
    let mut unwrapper = avt::util::TextUnwrapper::new();
    term.lines()
        .map(|line| unwrapper.push(line).is_none())
        .collect()
}
#[test]
fn full_line_erase_severs_incoming_wrap_and_preserves_next_line() {
    for erase in ["\x1b[K", "\x1b[2K"] {
        let mut term = vt(6, 4, true);
        term.feed_str("abcdefghijklm");
        term.feed_str(&format!("\x1b[2;1H{erase}"));
        assert_eq!(wrapped_rows(&term)[..3], [false, true, false]);
    }
}
#[test]
fn erased_wrapped_cursor_prefix_reflows_as_completed_output() {
    let mut term = vt(6, 3, true);
    term.feed_str("abcdefg\r\x1b[Kx");
    term.resize(3, 3);
    assert_eq!(view(&term), ["abc", "def", "x"]);
}
#[test]
fn partial_line_and_character_erase_keep_both_wrap_links() {
    for erase in ["\x1b[K", "\x1b[3X"] {
        let mut term = vt(6, 4, true);
        term.feed_str("abcdefghijklm");
        term.feed_str(&format!("\x1b[2;4H{erase}"));
        assert_eq!(wrapped_rows(&term)[..3], [true, true, false]);
    }
}
#[test]
fn display_erase_at_column_zero_severs_incoming_wrap() {
    let mut term = vt(6, 4, true);
    term.feed_str("abcdefghijklm\x1b[2;1H\x1b[J");
    assert_eq!(wrapped_rows(&term)[..3], [false, false, false]);
}
