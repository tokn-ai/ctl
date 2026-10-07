use avt::{Color, Vt};

fn interactive() -> Vt {
    Vt::builder()
        .size(60, 8)
        .scrollback_limit(100)
        .reflow_cursor_line(false)
        .build()
}

#[test]
fn resize_preserves_a_partial_control_sequence() {
    let mut term = interactive();
    term.feed_str("Restored session: Wed Oct 7\r\nclouds@Clouds-MacbookM2 ~ % \x1b[3");
    term.resize(22, 8);
    term.feed_str("4mX");
    let cursor = term.cursor();
    let cell = &term.line(cursor.row).cells()[21];
    assert_eq!(cell.char(), 'X');
    assert_eq!(cell.pen().foreground(), Some(Color::Indexed(4)));
}

#[test]
fn reset_retains_the_interactive_resize_policy() {
    let mut term = interactive();
    term.feed_str("discarded\x1bc");
    term.feed_str("Restored session: Wed Oct 7\r\nclouds@Clouds-MacbookM2 ~ % ");
    term.resize(22, 8);
    term.feed_str("\r\x1b[Jclouds@Clouds-MacbookM2 ~ % ");
    term.resize(60, 8);
    term.feed_str("\r\x1b[A\x1b[Jclouds@Clouds-MacbookM2 ~ % ");
    assert_eq!(
        term.line(0).text().trim_end(),
        "Restored session: Wed Oct 7"
    );
    assert_eq!(
        term.line(1).text().trim_end(),
        "clouds@Clouds-MacbookM2 ~ %"
    );
    assert_eq!(term.cursor(), (28, 1));
}

#[test]
fn styled_blank_rows_are_output_rather_than_unused_padding() {
    let mut term = Vt::builder().size(6, 3).reflow_cursor_line(false).build();
    term.feed_str("header\r\nx\x1b[3;1H\x1b[44m\x1b[2K\x1b[0m\x1b[2;2H");
    term.resize(3, 3);
    let colored_cells = term
        .lines()
        .flat_map(|line| line.cells())
        .filter(|cell| cell.pen().background() == Some(Color::Indexed(4)))
        .count();
    assert_eq!(colored_cells, 6);
}
