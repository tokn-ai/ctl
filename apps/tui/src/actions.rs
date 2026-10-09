use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ctmux_proto::{LeaseKind, ResizeDirection, SplitAxis};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
  Up,
  Down,
  Left,
  Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
  Split(SplitAxis),
  SwapPane {
    previous: bool,
    stay: bool,
  },
  BreakPane,
  Focus(Direction),
  ResizePane {
    direction: ResizeDirection,
    amount: u16,
  },
  NextPane,
  ToggleZoom,
  CreateSession,
  NextSession,
  PreviousSession,
  Sessions,
  Refresh,
  Archives,
  History {
    page_back: bool,
  },
  Paste,
  ToggleLease(LeaseKind),
  KillPane,
  Detach,
  Help,
  CommandPrompt,
  Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
  pub action: Action,
  pub repeatable: bool,
}

/// Resolve a prefix-table binding without discarding its modifiers or event kind.
///
/// Terminals may report uppercase letters and shifted punctuation with or without
/// `SHIFT`. Normalize the modifier only for characters: shifted arrows are separate
/// bindings, and control/alt combinations must never invoke an unmodified command.
pub fn resolve(key: KeyEvent) -> Option<Binding> {
  if key.kind == KeyEventKind::Release {
    return None;
  }
  if let Some(direction) = match key.code {
    KeyCode::Up => Some(ResizeDirection::Up),
    KeyCode::Down => Some(ResizeDirection::Down),
    KeyCode::Left => Some(ResizeDirection::Left),
    KeyCode::Right => Some(ResizeDirection::Right),
    _ => None,
  } {
    let amount = match key.modifiers {
      KeyModifiers::CONTROL => Some(1),
      KeyModifiers::ALT => Some(5),
      _ => None,
    };
    if let Some(amount) = amount {
      return Some(Binding {
        action: Action::ResizePane { direction, amount },
        repeatable: true,
      });
    }
  }
  let code = if key.modifiers.is_empty() {
    key.code
  } else if key.modifiers == KeyModifiers::SHIFT {
    let KeyCode::Char(ch) = key.code else {
      return None;
    };
    KeyCode::Char(ch.to_ascii_uppercase())
  } else {
    return None;
  };
  let action = match code {
    KeyCode::Char('%') => Action::Split(SplitAxis::Horizontal),
    KeyCode::Char('"') => Action::Split(SplitAxis::Vertical),
    KeyCode::Char('{') => Action::SwapPane {
      previous: true,
      stay: false,
    },
    KeyCode::Char('}') => Action::SwapPane {
      previous: false,
      stay: false,
    },
    KeyCode::Char('!') => Action::BreakPane,
    KeyCode::Up => Action::Focus(Direction::Up),
    KeyCode::Down => Action::Focus(Direction::Down),
    KeyCode::Left => Action::Focus(Direction::Left),
    KeyCode::Right => Action::Focus(Direction::Right),
    KeyCode::Char('o') => Action::NextPane,
    KeyCode::Char('z') => Action::ToggleZoom,
    KeyCode::Char('c') => Action::CreateSession,
    KeyCode::Char('n') => Action::NextSession,
    KeyCode::Char('p') => Action::PreviousSession,
    KeyCode::Char('s' | 'w') => Action::Sessions,
    KeyCode::Char('r') => Action::Refresh,
    KeyCode::Char('A') => Action::Archives,
    KeyCode::Char('[') => Action::History { page_back: false },
    KeyCode::PageUp => Action::History { page_back: true },
    KeyCode::Char(']') => Action::Paste,
    KeyCode::Char('I') => Action::ToggleLease(LeaseKind::Input),
    KeyCode::Char('R') => Action::ToggleLease(LeaseKind::Layout),
    KeyCode::Char('x') => Action::KillPane,
    KeyCode::Char('d') => Action::Detach,
    KeyCode::Char('?') => Action::Help,
    KeyCode::Char(':') => Action::CommandPrompt,
    KeyCode::Esc => Action::Cancel,
    _ => return None,
  };
  Some(Binding {
    action,
    // Pane focus and resizing share tmux's repeat table. Other commands still
    // require their own prefix so an ordinary subsequent key stays input.
    repeatable: matches!(action, Action::Focus(_)),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
  }

  #[test]
  fn existing_prefix_commands_resolve_to_typed_actions() {
    let bindings = [
      (KeyCode::Char('%'), Action::Split(SplitAxis::Horizontal)),
      (KeyCode::Char('"'), Action::Split(SplitAxis::Vertical)),
      (
        KeyCode::Char('{'),
        Action::SwapPane {
          previous: true,
          stay: false,
        },
      ),
      (
        KeyCode::Char('}'),
        Action::SwapPane {
          previous: false,
          stay: false,
        },
      ),
      (KeyCode::Char('!'), Action::BreakPane),
      (KeyCode::Char('o'), Action::NextPane),
      (KeyCode::Char('z'), Action::ToggleZoom),
      (KeyCode::Char('c'), Action::CreateSession),
      (KeyCode::Char('n'), Action::NextSession),
      (KeyCode::Char('p'), Action::PreviousSession),
      (KeyCode::Char('s'), Action::Sessions),
      (KeyCode::Char('w'), Action::Sessions),
      (KeyCode::Char('r'), Action::Refresh),
      (KeyCode::Char('A'), Action::Archives),
      (KeyCode::Char('['), Action::History { page_back: false }),
      (KeyCode::PageUp, Action::History { page_back: true }),
      (KeyCode::Char(']'), Action::Paste),
      (KeyCode::Char('I'), Action::ToggleLease(LeaseKind::Input)),
      (KeyCode::Char('R'), Action::ToggleLease(LeaseKind::Layout)),
      (KeyCode::Char('x'), Action::KillPane),
      (KeyCode::Char('d'), Action::Detach),
      (KeyCode::Char('?'), Action::Help),
      (KeyCode::Char(':'), Action::CommandPrompt),
      (KeyCode::Esc, Action::Cancel),
    ];
    for (code, action) in bindings {
      assert_eq!(
        resolve(key(code, KeyModifiers::NONE)),
        Some(Binding {
          action,
          repeatable: false,
        }),
        "{code:?}"
      );
    }
    assert_eq!(resolve(key(KeyCode::Char('u'), KeyModifiers::NONE)), None);
  }

  #[test]
  fn only_unmodified_arrows_are_repeatable_focus_bindings() {
    for (code, direction) in [
      (KeyCode::Up, Direction::Up),
      (KeyCode::Down, Direction::Down),
      (KeyCode::Left, Direction::Left),
      (KeyCode::Right, Direction::Right),
    ] {
      assert_eq!(
        resolve(key(code, KeyModifiers::NONE)),
        Some(Binding {
          action: Action::Focus(direction),
          repeatable: true,
        })
      );
      for modifiers in [
        KeyModifiers::SHIFT,
        KeyModifiers::ALT | KeyModifiers::CONTROL,
      ] {
        assert_eq!(
          resolve(key(code, modifiers)),
          None,
          "{code:?} {modifiers:?}"
        );
      }
    }
  }

  #[test]
  fn control_and_alt_arrows_resize_with_repeatable_cell_steps() {
    for (code, direction) in [
      (KeyCode::Up, ResizeDirection::Up),
      (KeyCode::Down, ResizeDirection::Down),
      (KeyCode::Left, ResizeDirection::Left),
      (KeyCode::Right, ResizeDirection::Right),
    ] {
      for (modifiers, amount) in [(KeyModifiers::CONTROL, 1), (KeyModifiers::ALT, 5)] {
        assert_eq!(
          resolve(key(code, modifiers)),
          Some(Binding {
            action: Action::ResizePane { direction, amount },
            repeatable: true,
          })
        );
      }
    }
  }

  #[test]
  fn shifted_letters_and_symbols_keep_their_distinct_commands() {
    for (ch, action) in [
      ('A', Action::Archives),
      ('a', Action::Archives),
      ('I', Action::ToggleLease(LeaseKind::Input)),
      ('i', Action::ToggleLease(LeaseKind::Input)),
      ('R', Action::ToggleLease(LeaseKind::Layout)),
      ('r', Action::ToggleLease(LeaseKind::Layout)),
      ('?', Action::Help),
      ('"', Action::Split(SplitAxis::Vertical)),
      ('%', Action::Split(SplitAxis::Horizontal)),
      (
        '{',
        Action::SwapPane {
          previous: true,
          stay: false,
        },
      ),
      (
        '}',
        Action::SwapPane {
          previous: false,
          stay: false,
        },
      ),
      ('!', Action::BreakPane),
    ] {
      assert_eq!(
        resolve(key(KeyCode::Char(ch), KeyModifiers::SHIFT)).map(|binding| binding.action),
        Some(action),
        "shifted {ch}"
      );
    }
    assert_eq!(resolve(key(KeyCode::Char('d'), KeyModifiers::SHIFT)), None);
    assert_eq!(resolve(key(KeyCode::PageUp, KeyModifiers::SHIFT)), None);
    assert_eq!(resolve(key(KeyCode::Esc, KeyModifiers::SHIFT)), None);
  }

  #[test]
  fn control_and_alt_commands_never_alias_plain_bindings() {
    for modifiers in [
      KeyModifiers::CONTROL,
      KeyModifiers::ALT,
      KeyModifiers::CONTROL | KeyModifiers::SHIFT,
      KeyModifiers::ALT | KeyModifiers::SHIFT,
      KeyModifiers::SUPER,
      KeyModifiers::HYPER,
      KeyModifiers::META,
    ] {
      for ch in ['d', 'x', 'c', '%', '"', '{', '}', '!', 'I', 'R', '?'] {
        assert_eq!(
          resolve(key(KeyCode::Char(ch), modifiers)),
          None,
          "{ch} {modifiers:?}"
        );
      }
    }
  }

  #[test]
  fn release_is_ignored_and_press_and_repeat_resolve() {
    for (kind, expected) in [
      (KeyEventKind::Press, Some(Action::Detach)),
      (KeyEventKind::Repeat, Some(Action::Detach)),
      (KeyEventKind::Release, None),
    ] {
      let mut event = key(KeyCode::Char('d'), KeyModifiers::NONE);
      event.kind = kind;
      assert_eq!(resolve(event).map(|binding| binding.action), expected);
    }
    let mut arrow = key(KeyCode::Right, KeyModifiers::NONE);
    arrow.kind = KeyEventKind::Repeat;
    assert_eq!(
      resolve(arrow),
      Some(Binding {
        action: Action::Focus(Direction::Right),
        repeatable: true,
      })
    );
  }

  #[test]
  fn pane_move_bindings_ignore_releases_and_never_enter_the_repeat_table() {
    for (code, action) in [
      (
        '{',
        Action::SwapPane {
          previous: true,
          stay: false,
        },
      ),
      (
        '}',
        Action::SwapPane {
          previous: false,
          stay: false,
        },
      ),
      ('!', Action::BreakPane),
    ] {
      for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
        for kind in [
          KeyEventKind::Press,
          KeyEventKind::Repeat,
          KeyEventKind::Release,
        ] {
          let event = KeyEvent::new_with_kind(KeyCode::Char(code), modifiers, kind);
          assert_eq!(
            resolve(event),
            (kind != KeyEventKind::Release).then_some(Binding {
              action,
              repeatable: false
            })
          );
        }
      }
    }
  }
}
