use crate::actions::{Action, Direction};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ctmux_proto::{LeaseKind, ResizeDirection, SplitAxis};
use std::collections::VecDeque;
use unicode_width::UnicodeWidthChar;

const MAX_TEXT_BYTES: usize = 4096;
const MAX_HISTORY: usize = 100;
const COMMAND_NAMES: &[&str] = &[
  "split-window",
  "select-pane",
  "resize-pane",
  "new-session",
  "switch-client",
  "list-sessions",
  "kill-pane",
  "copy-mode",
  "paste-buffer",
  "refresh-client",
  "detach-client",
  "list-keys",
  "take-input",
  "release-input",
  "take-resize",
  "release-resize",
];

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
  Stay,
  Closed,
  Submit(String),
}

/// Client-local text entry. The cursor is always a UTF-8 character boundary.
#[derive(Default)]
pub(crate) struct Prompt {
  active: bool,
  text: String,
  cursor: usize,
  history: VecDeque<String>,
  history_index: Option<usize>,
  draft: String,
  draft_cursor: usize,
}

impl Prompt {
  pub fn open(&mut self) {
    self.active = true;
    self.text.clear();
    self.cursor = 0;
    self.history_index = None;
    self.draft.clear();
    self.draft_cursor = 0;
  }

  pub fn is_active(&self) -> bool {
    self.active
  }

  pub fn key(&mut self, key: KeyEvent) -> Event {
    if !self.active || key.kind == KeyEventKind::Release {
      return Event::Stay;
    }
    if key.modifiers == KeyModifiers::CONTROL
      || key.modifiers == KeyModifiers::CONTROL | KeyModifiers::SHIFT
    {
      return self.control_key(key.code);
    }
    if !key.modifiers.is_empty() && key.modifiers != KeyModifiers::SHIFT {
      return Event::Stay;
    }
    match key.code {
      KeyCode::Enter => return self.submit(),
      KeyCode::Esc => return self.close(),
      KeyCode::Left => self.cursor = previous_boundary(&self.text, self.cursor),
      KeyCode::Right => self.cursor = next_boundary(&self.text, self.cursor),
      KeyCode::Home => self.cursor = 0,
      KeyCode::End => self.cursor = self.text.len(),
      KeyCode::Backspace => {
        let previous = previous_boundary(&self.text, self.cursor);
        self.remove(previous, self.cursor);
      }
      KeyCode::Delete => self.remove(self.cursor, next_boundary(&self.text, self.cursor)),
      KeyCode::Up => self.previous_history(),
      KeyCode::Down => self.next_history(),
      KeyCode::Tab => self.complete(),
      KeyCode::Char(ch) if !ch.is_control() => self.insert(&ch.to_string()),
      _ => {}
    }
    Event::Stay
  }

  fn control_key(&mut self, code: KeyCode) -> Event {
    let KeyCode::Char(ch) = code else {
      return Event::Stay;
    };
    match ch.to_ascii_lowercase() {
      'c' | 'g' => return self.close(),
      'a' => self.cursor = 0,
      'e' => self.cursor = self.text.len(),
      'b' => self.cursor = previous_boundary(&self.text, self.cursor),
      'f' => self.cursor = next_boundary(&self.text, self.cursor),
      'u' => self.remove(0, self.cursor),
      'k' => self.remove(self.cursor, self.text.len()),
      'w' => {
        let mut start = self.cursor;
        while start > 0
          && self.text[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
        {
          start = previous_boundary(&self.text, start);
        }
        while start > 0
          && self.text[..start]
            .chars()
            .next_back()
            .is_some_and(|ch| !ch.is_whitespace())
        {
          start = previous_boundary(&self.text, start);
        }
        self.remove(start, self.cursor);
      }
      _ => {}
    }
    Event::Stay
  }

  /// Bracketed paste is text entry, including when it contains a newline.
  pub fn paste(&mut self, text: &str) {
    if !self.active {
      return;
    }
    let text: String = text
      .chars()
      .filter_map(|ch| match ch {
        '\n' | '\r' | '\t' => Some(' '),
        ch if ch.is_control() => None,
        ch => Some(ch),
      })
      .take(MAX_TEXT_BYTES)
      .collect();
    self.insert(&text);
  }

  fn insert(&mut self, text: &str) {
    let remaining = MAX_TEXT_BYTES.saturating_sub(self.text.len());
    let mut bytes = 0;
    for ch in text.chars() {
      if bytes + ch.len_utf8() > remaining {
        break;
      }
      bytes += ch.len_utf8();
    }
    if bytes > 0 {
      self.edited();
      self.text.insert_str(self.cursor, &text[..bytes]);
      self.cursor += bytes;
    }
  }

  fn remove(&mut self, start: usize, end: usize) {
    if start < end {
      self.edited();
      self.text.replace_range(start..end, "");
      self.cursor = start;
    }
  }

  fn edited(&mut self) {
    self.history_index = None;
    self.draft.clear();
  }

  fn submit(&mut self) -> Event {
    self.active = false;
    self.history_index = None;
    if !self.text.trim().is_empty() && self.history.back() != Some(&self.text) {
      self.history.push_back(self.text.clone());
      if self.history.len() > MAX_HISTORY {
        self.history.pop_front();
      }
    }
    Event::Submit(self.text.clone())
  }

  fn close(&mut self) -> Event {
    self.active = false;
    self.history_index = None;
    Event::Closed
  }

  fn previous_history(&mut self) {
    if self.history.is_empty() {
      return;
    }
    let index = if let Some(index) = self.history_index {
      index.saturating_sub(1)
    } else {
      self.draft.clone_from(&self.text);
      self.draft_cursor = self.cursor;
      self.history.len() - 1
    };
    self.history_index = Some(index);
    self.text.clone_from(&self.history[index]);
    self.cursor = self.text.len();
  }

  fn next_history(&mut self) {
    let Some(index) = self.history_index else {
      return;
    };
    if index + 1 < self.history.len() {
      self.history_index = Some(index + 1);
      self.text.clone_from(&self.history[index + 1]);
      self.cursor = self.text.len();
    } else {
      self.history_index = None;
      self.text.clone_from(&self.draft);
      self.cursor = self.draft_cursor;
    }
  }

  fn complete(&mut self) {
    if self.cursor != self.text.len() || self.text.chars().any(char::is_whitespace) {
      return;
    }
    if command_name(&self.text).is_some() {
      self.insert(" ");
      return;
    }
    let candidates: Vec<_> = COMMAND_NAMES
      .iter()
      .filter(|name| name.starts_with(&self.text))
      .copied()
      .collect();
    let Some(first) = candidates.first() else {
      return;
    };
    let length = candidates.iter().fold(first.len(), |length, candidate| {
      first[..length]
        .bytes()
        .zip(candidate.bytes())
        .take_while(|(a, b)| a == b)
        .count()
    });
    // Command names are ASCII. Aliases identify their full command instead of
    // competing with it when completing a partial canonical name.
    self.edited();
    self.text = first[..length].to_owned();
    self.cursor = self.text.len();
    if candidates.len() == 1 {
      self.insert(" ");
    }
  }

  /// Keep the insertion cell visible without clipping a wide character or
  /// leaving its combining marks at the beginning of the footer.
  pub fn display(&self, columns: u16) -> (String, u16) {
    if columns == 0 {
      return (String::new(), 0);
    }
    if columns == 1 {
      return (":".into(), 0);
    }
    let width = usize::from(columns - 1);
    let cursor_column: usize = self.text[..self.cursor].chars().map(character_width).sum();
    let target = cursor_column.saturating_sub(width - 1);
    let groups = display_groups(&self.text);
    let mut start = 0;
    let mut start_column = 0;
    while let Some((_, cells)) = groups.get(start) {
      if start_column >= target || start_column + cells > cursor_column {
        break;
      }
      start_column += cells;
      start += 1;
    }
    let mut text = String::from(":");
    let mut used = 0;
    for (group, cells) in &groups[start..] {
      if used + cells > width {
        break;
      }
      text.push_str(group);
      used += cells;
    }
    let cursor = 1 + cursor_column.saturating_sub(start_column).min(width - 1);
    (text, u16::try_from(cursor).expect("bounded prompt column"))
  }
}

fn previous_boundary(text: &str, cursor: usize) -> usize {
  text[..cursor]
    .char_indices()
    .next_back()
    .map_or(0, |(index, _)| index)
}

fn next_boundary(text: &str, cursor: usize) -> usize {
  cursor + text[cursor..].chars().next().map_or(0, char::len_utf8)
}

fn character_width(ch: char) -> usize {
  ch.width().unwrap_or(0)
}

fn display_groups(text: &str) -> Vec<(&str, usize)> {
  let mut groups: Vec<(&str, usize)> = Vec::new();
  let mut start = None;
  let mut cells = 0;
  for (index, ch) in text.char_indices() {
    let width = character_width(ch);
    if width > 0 {
      if let Some(start) = start {
        groups.push((&text[start..index], cells));
      }
      start = Some(index);
      cells = width;
    }
  }
  if let Some(start) = start {
    groups.push((&text[start..], cells));
  }
  groups
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
  Action(Action),
  NewSession(Option<String>),
  SwitchSession(String),
  Lease { kind: LeaseKind, requested: bool },
}

pub(crate) fn parse(text: &str) -> Result<Command, String> {
  let words = tokenize(text)?;
  let Some(name) = words.first() else {
    return Ok(Command::Action(Action::Cancel));
  };
  let Some(name) = command_name(name) else {
    return Err(format!(
      "Unknown command: {name}; use list-keys for supported commands"
    ));
  };
  let args = &words[1..];
  match name {
    "split-window" => match args
      .iter()
      .map(String::as_str)
      .collect::<Vec<_>>()
      .as_slice()
    {
      [] | ["-v"] => Ok(Command::Action(Action::Split(SplitAxis::Vertical))),
      ["-h"] => Ok(Command::Action(Action::Split(SplitAxis::Horizontal))),
      _ => Err(usage("split-window [-h|-v]")),
    },
    "select-pane" => {
      if args.len() != 1 {
        return Err(usage("select-pane -L|-R|-U|-D"));
      }
      let direction = match args[0].as_str() {
        "-L" => Direction::Left,
        "-R" => Direction::Right,
        "-U" => Direction::Up,
        "-D" => Direction::Down,
        _ => return Err(usage("select-pane -L|-R|-U|-D")),
      };
      Ok(Command::Action(Action::Focus(direction)))
    }
    "resize-pane" => resize_command(args),
    "new-session" => match args
      .iter()
      .map(String::as_str)
      .collect::<Vec<_>>()
      .as_slice()
    {
      [] => Ok(Command::NewSession(None)),
      ["-s", name] if !name.is_empty() => Ok(Command::NewSession(Some((*name).into()))),
      _ => Err(usage("new-session [-s NAME]")),
    },
    "switch-client" => match args
      .iter()
      .map(String::as_str)
      .collect::<Vec<_>>()
      .as_slice()
    {
      ["-n"] => Ok(Command::Action(Action::NextSession)),
      ["-p"] => Ok(Command::Action(Action::PreviousSession)),
      ["-t", name] if !name.is_empty() => Ok(Command::SwitchSession((*name).into())),
      _ => Err(usage("switch-client -n|-p|-t NAME")),
    },
    "copy-mode" => match args
      .iter()
      .map(String::as_str)
      .collect::<Vec<_>>()
      .as_slice()
    {
      [] => Ok(Command::Action(Action::History { page_back: false })),
      ["-u"] => Ok(Command::Action(Action::History { page_back: true })),
      _ => Err(usage("copy-mode [-u]")),
    },
    name => no_argument_command(name, args),
  }
}

fn command_name(name: &str) -> Option<&'static str> {
  Some(match name {
    "split-window" | "splitw" => "split-window",
    "select-pane" | "selectp" => "select-pane",
    "resize-pane" | "resizep" => "resize-pane",
    "new-session" | "new" => "new-session",
    "switch-client" | "switchc" => "switch-client",
    "list-sessions" | "ls" => "list-sessions",
    "kill-pane" | "killp" => "kill-pane",
    "copy-mode" => "copy-mode",
    "paste-buffer" | "pasteb" => "paste-buffer",
    "refresh-client" | "refresh" => "refresh-client",
    "detach-client" | "detach" => "detach-client",
    "list-keys" | "lsk" => "list-keys",
    "take-input" => "take-input",
    "release-input" => "release-input",
    "take-resize" => "take-resize",
    "release-resize" => "release-resize",
    _ => return None,
  })
}

fn resize_command(args: &[String]) -> Result<Command, String> {
  let usage = || usage("resize-pane -L|-R|-U|-D [CELLS], or resize-pane -Z");
  if args.len() == 1 && args[0] == "-Z" {
    return Ok(Command::Action(Action::ToggleZoom));
  }
  if args.is_empty() || args.len() > 2 {
    return Err(usage());
  }
  let direction = match args[0].as_str() {
    "-L" => ResizeDirection::Left,
    "-R" => ResizeDirection::Right,
    "-U" => ResizeDirection::Up,
    "-D" => ResizeDirection::Down,
    _ => return Err(usage()),
  };
  let amount = if let Some(amount) = args.get(1) {
    amount
      .parse::<u16>()
      .ok()
      .filter(|amount| *amount > 0)
      .ok_or("Resize cells must be an integer from 1 to 65535")?
  } else {
    1
  };
  Ok(Command::Action(Action::ResizePane { direction, amount }))
}

fn no_argument_command(name: &str, args: &[String]) -> Result<Command, String> {
  if !args.is_empty() {
    return Err(usage(name));
  }
  let action = match name {
    "list-sessions" => Action::Sessions,
    "kill-pane" => Action::KillPane,
    "paste-buffer" => Action::Paste,
    "refresh-client" => Action::Refresh,
    "detach-client" => Action::Detach,
    "list-keys" => Action::Help,
    "take-input" | "release-input" | "take-resize" | "release-resize" => {
      return Ok(Command::Lease {
        kind: if name.ends_with("input") {
          LeaseKind::Input
        } else {
          LeaseKind::Layout
        },
        requested: name.starts_with("take"),
      });
    }
    _ => return Err("Unsupported command".into()),
  };
  Ok(Command::Action(action))
}

fn usage(command: &str) -> String {
  format!("Unsupported arguments; usage: {command}")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
  None,
  Single,
  Double,
}

/// Tokenize one literal control command, without evaluating a shell language.
fn tokenize(text: &str) -> Result<Vec<String>, String> {
  if text.len() > MAX_TEXT_BYTES {
    return Err(format!("Commands are limited to {MAX_TEXT_BYTES} bytes"));
  }
  if text.chars().any(char::is_control) {
    return Err("Control characters and newlines are not supported in commands".into());
  }
  let mut words = Vec::new();
  let mut word = String::new();
  let mut started = false;
  let mut quote = Quote::None;
  let mut chars = text.chars();
  while let Some(ch) = chars.next() {
    match (quote, ch) {
      (Quote::Single, '\'') | (Quote::Double, '"') => quote = Quote::None,
      (Quote::Single, ch) => word.push(ch),
      (Quote::None, '\'') => {
        started = true;
        quote = Quote::Single;
      }
      (Quote::None, '"') => {
        started = true;
        quote = Quote::Double;
      }
      (Quote::None | Quote::Double, '\\') => {
        word.push(chars.next().ok_or("Trailing backslash in command")?);
        started = true;
      }
      (Quote::None | Quote::Double, '$' | '`') => {
        return Err("Command expansions are not supported; quote or escape literal names".into());
      }
      (Quote::None, ';' | '|' | '&' | '<' | '>' | '{' | '}' | '#') => {
        return Err(
          "Enter one command; command chains, redirection, and formats are not supported".into(),
        );
      }
      (Quote::None, '*' | '?' | '[' | ']') => {
        return Err("Target patterns are not supported; quote or escape literal names".into());
      }
      (Quote::None, '~') if !started => {
        return Err("Command expansions are not supported; quote or escape literal names".into());
      }
      (Quote::None, ch) if ch.is_whitespace() => {
        if started {
          words.push(std::mem::take(&mut word));
          started = false;
        }
      }
      (_, ch) => {
        word.push(ch);
        started = true;
      }
    }
  }
  match quote {
    Quote::Single => return Err("Unterminated single quote".into()),
    Quote::Double => return Err("Unterminated double quote".into()),
    Quote::None => {}
  }
  if started {
    words.push(word);
  }
  Ok(words)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
  }

  fn control(ch: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
  }

  fn enter(prompt: &mut Prompt, text: &str) {
    prompt.open();
    prompt.paste(text);
    assert_eq!(prompt.key(key(KeyCode::Enter)), Event::Submit(text.into()));
  }

  #[test]
  fn aliases_map_to_the_same_actions_and_preserve_split_and_zoom_semantics() {
    for (names, suffix, expected) in [
      (
        ["split-window", "splitw"],
        "",
        Command::Action(Action::Split(SplitAxis::Vertical)),
      ),
      (
        ["select-pane", "selectp"],
        " -R",
        Command::Action(Action::Focus(Direction::Right)),
      ),
      (
        ["resize-pane", "resizep"],
        " -Z",
        Command::Action(Action::ToggleZoom),
      ),
      (
        ["new-session", "new"],
        " -s work",
        Command::NewSession(Some("work".into())),
      ),
      (
        ["switch-client", "switchc"],
        " -t work",
        Command::SwitchSession("work".into()),
      ),
      (
        ["list-sessions", "ls"],
        "",
        Command::Action(Action::Sessions),
      ),
      (
        ["kill-pane", "killp"],
        "",
        Command::Action(Action::KillPane),
      ),
      (
        ["paste-buffer", "pasteb"],
        "",
        Command::Action(Action::Paste),
      ),
      (
        ["refresh-client", "refresh"],
        "",
        Command::Action(Action::Refresh),
      ),
      (
        ["detach-client", "detach"],
        "",
        Command::Action(Action::Detach),
      ),
      (["list-keys", "lsk"], "", Command::Action(Action::Help)),
    ] {
      for name in names {
        assert_eq!(parse(&format!("{name}{suffix}")), Ok(expected.clone()));
      }
    }
    assert_eq!(
      parse("splitw -h"),
      Ok(Command::Action(Action::Split(SplitAxis::Horizontal)))
    );
    assert_eq!(
      parse("copy-mode -u"),
      Ok(Command::Action(Action::History { page_back: true }))
    );
    assert_eq!(
      parse("copy-mode"),
      Ok(Command::Action(Action::History { page_back: false }))
    );
    assert_eq!(parse("new"), Ok(Command::NewSession(None)));
    assert_eq!(
      parse("switchc -n"),
      Ok(Command::Action(Action::NextSession))
    );
    assert_eq!(
      parse("switchc -p"),
      Ok(Command::Action(Action::PreviousSession))
    );
  }

  #[test]
  fn resize_and_ownership_commands_use_bounded_values_and_explicit_intent() {
    for (flag, direction) in [
      ("-L", ResizeDirection::Left),
      ("-R", ResizeDirection::Right),
      ("-U", ResizeDirection::Up),
      ("-D", ResizeDirection::Down),
    ] {
      assert_eq!(
        parse(&format!("resizep {flag}")),
        Ok(Command::Action(Action::ResizePane {
          direction,
          amount: 1
        }))
      );
      assert_eq!(
        parse(&format!("resizep {flag} 65535")),
        Ok(Command::Action(Action::ResizePane {
          direction,
          amount: 65535
        }))
      );
    }
    for (name, kind, requested) in [
      ("take-input", LeaseKind::Input, true),
      ("release-input", LeaseKind::Input, false),
      ("take-resize", LeaseKind::Layout, true),
      ("release-resize", LeaseKind::Layout, false),
    ] {
      assert_eq!(parse(name), Ok(Command::Lease { kind, requested }));
    }
    for amount in ["0", "-1", "65536", "1.5", "ten"] {
      assert!(
        parse(&format!("resizep -R {amount}"))
          .unwrap_err()
          .contains("integer")
      );
    }
  }

  #[test]
  fn parser_accepts_literal_quoted_names_and_backslash_escapes() {
    for text in ["new -s 'my work'", "new -s \"my work\"", "new -s my\\ work"] {
      assert_eq!(parse(text), Ok(Command::NewSession(Some("my work".into()))));
    }
    assert_eq!(
      parse("new -s pre' mid'\"dle\""),
      Ok(Command::NewSession(Some("pre middle".into())))
    );
    assert_eq!(
      parse("switchc -t 'semi;colon $literal'"),
      Ok(Command::SwitchSession("semi;colon $literal".into()))
    );
    assert_eq!(
      parse("switchc -t literal\\;name"),
      Ok(Command::SwitchSession("literal;name".into()))
    );
    for text in ["new -s 'unfinished", "new -s \"unfinished", "new -s name\\"] {
      assert!(parse(text).is_err());
    }
  }

  #[test]
  fn unsupported_flags_commands_and_shell_languages_never_fall_through() {
    for text in [
      "echo hello",
      "run-shell ls",
      "set-option mouse on",
      "select-pane",
      "selectp -L -R",
      "splitw -hv",
      "resizep -Z 2",
      "resizep -z",
      "resizep -R -L",
      "new -s",
      "new -s ''",
      "switchc -n -p",
      "copy-mode -q",
      "ls -a",
      "detach -a",
      "take-input -f",
      "splitw; detach",
      "ls | cat",
      "ls && detach",
      "ls > file",
      "new -s $HOME",
      "new -s \"$HOME\"",
      "new -s `hostname`",
      "new -s ~/work",
      "switchc -t work*",
      "new -s #{session_name}",
      "ls\ndetach",
      "ls\x1b[2J",
    ] {
      assert!(parse(text).is_err(), "{text:?}");
    }
    assert_eq!(parse("  "), Ok(Command::Action(Action::Cancel)));
  }

  #[test]
  fn editor_keeps_utf8_boundaries_and_supports_emacs_deletion() {
    let mut prompt = Prompt::default();
    prompt.open();
    prompt.paste("ab界cd");
    prompt.key(key(KeyCode::Left));
    prompt.key(key(KeyCode::Left));
    prompt.key(key(KeyCode::Backspace));
    assert_eq!(prompt.text, "abcd");
    prompt.key(key(KeyCode::Char('文')));
    prompt.key(control('b'));
    prompt.key(key(KeyCode::Delete));
    assert_eq!(prompt.text, "abcd");
    prompt.key(control('a'));
    prompt.key(control('f'));
    prompt.key(control('k'));
    assert_eq!(prompt.text, "a");
    prompt.paste(" word two  ");
    prompt.key(control('w'));
    assert_eq!(prompt.text, "a word ");
    prompt.key(control('u'));
    assert_eq!(prompt.text, "");
    prompt.paste("end");
    prompt.key(key(KeyCode::Home));
    prompt.key(key(KeyCode::Char('界')));
    prompt.key(key(KeyCode::End));
    prompt.key(control('e'));
    assert_eq!(
      prompt.key(key(KeyCode::Enter)),
      Event::Submit("界end".into())
    );
  }

  #[test]
  fn paste_cannot_submit_or_inject_controls_and_text_is_bounded_by_bytes() {
    let mut prompt = Prompt::default();
    prompt.open();
    prompt.paste("splitw\n-h\r\t\x1b\0\x7f");
    assert!(prompt.is_active());
    assert_eq!(prompt.text, "splitw -h  ");
    prompt.open();
    prompt.paste(&"界".repeat(2000));
    assert_eq!(prompt.text.len(), 4095);
    prompt.key(key(KeyCode::Char('x')));
    assert_eq!(prompt.text.len(), MAX_TEXT_BYTES);
    prompt.key(key(KeyCode::Char('y')));
    assert_eq!(prompt.text.len(), MAX_TEXT_BYTES);
    assert!(parse(&"x".repeat(MAX_TEXT_BYTES + 1)).is_err());
  }

  #[test]
  fn history_survives_opens_is_bounded_and_restores_the_unsubmitted_draft() {
    let mut prompt = Prompt::default();
    for index in 0..105 {
      enter(&mut prompt, &format!("new -s session-{index}"));
    }
    enter(&mut prompt, "new -s session-104");
    assert_eq!(prompt.history.len(), MAX_HISTORY);
    assert_eq!(prompt.history.front().unwrap(), "new -s session-5");
    prompt.open();
    prompt.paste("draft界");
    prompt.key(key(KeyCode::Left));
    let cursor = prompt.cursor;
    prompt.key(key(KeyCode::Up));
    assert_eq!(prompt.text, "new -s session-104");
    prompt.key(key(KeyCode::Up));
    assert_eq!(prompt.text, "new -s session-103");
    prompt.key(key(KeyCode::Down));
    prompt.key(key(KeyCode::Down));
    assert_eq!(prompt.text, "draft界");
    assert_eq!(prompt.cursor, cursor);
    prompt.key(key(KeyCode::Up));
    prompt.key(key(KeyCode::Char('x')));
    let edited = prompt.text.clone();
    prompt.key(key(KeyCode::Up));
    prompt.key(key(KeyCode::Down));
    assert_eq!(prompt.text, edited);
  }

  #[test]
  fn completion_only_edits_the_command_name_and_leaves_ambiguous_prefixes() {
    let mut prompt = Prompt::default();
    prompt.open();
    prompt.paste("spl");
    prompt.key(key(KeyCode::Tab));
    assert_eq!(prompt.text, "split-window ");
    prompt.open();
    prompt.paste("take-");
    prompt.key(key(KeyCode::Tab));
    assert_eq!(prompt.text, "take-");
    prompt.open();
    prompt.paste("splitw");
    prompt.key(key(KeyCode::Tab));
    assert_eq!(prompt.text, "splitw ");
    prompt.paste("-h");
    prompt.key(key(KeyCode::Tab));
    assert_eq!(prompt.text, "splitw -h");
  }

  #[test]
  fn display_clips_wide_cells_combining_groups_and_scrolls_with_cursor() {
    let mut prompt = Prompt::default();
    prompt.open();
    prompt.paste("abcd");
    assert_eq!(prompt.display(5), (":bcd".into(), 4));
    prompt.key(key(KeyCode::Home));
    assert_eq!(prompt.display(5), (":abcd".into(), 1));
    prompt.open();
    prompt.paste("中文x");
    assert_eq!(prompt.display(4), (":x".into(), 2));
    prompt.key(key(KeyCode::Home));
    assert_eq!(prompt.display(4), (":中".into(), 1));
    prompt.open();
    prompt.paste("e\u{301}界x");
    assert_eq!(prompt.display(4), (":x".into(), 2));
    prompt.key(key(KeyCode::Home));
    assert_eq!(prompt.display(4), (":e\u{301}界".into(), 1));
    for columns in 0..9 {
      let (text, cursor) = prompt.display(columns);
      assert!(text.chars().map(character_width).sum::<usize>() <= usize::from(columns));
      assert!(columns == 0 || cursor < columns);
    }
    assert_eq!(prompt.display(0), (String::new(), 0));
    assert_eq!(prompt.display(1), (":".into(), 0));
  }

  #[test]
  fn escape_interrupts_release_and_blank_submission_do_not_change_history() {
    let mut prompt = Prompt::default();
    enter(&mut prompt, "ls");
    for cancel in [key(KeyCode::Esc), control('c'), control('g')] {
      prompt.open();
      prompt.paste("detach");
      assert_eq!(prompt.key(cancel), Event::Closed);
      assert!(!prompt.is_active());
      assert_eq!(prompt.history.len(), 1);
    }
    prompt.open();
    let mut release = key(KeyCode::Char('x'));
    release.kind = KeyEventKind::Release;
    assert_eq!(prompt.key(release), Event::Stay);
    assert_eq!(
      prompt.key(key(KeyCode::Enter)),
      Event::Submit(String::new())
    );
    assert_eq!(prompt.history.len(), 1);
    prompt.paste("ignored");
    assert_eq!(prompt.text, "");
  }
}
