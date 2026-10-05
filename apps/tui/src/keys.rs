//! Prefix and repeat-table transitions, independent of terminal or daemon I/O.
use crate::{
  actions::{self, Binding},
  input::{self, Prefix},
};
use crossterm::event::{KeyEvent, KeyEventKind};
use std::time::Duration;
use tokio::time::Instant;

const REPEAT_TIME: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyState {
  #[default]
  Root,
  Prefix,
  Repeat {
    until: Instant,
  },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
  Ignore,
  Pending,
  Action(Binding),
  SendPrefix,
  Unknown,
  Forward,
}

impl KeyState {
  pub fn is_prefix(self) -> bool {
    matches!(self, Self::Prefix)
  }

  /// Text entry or a new mouse gesture leaves repeat mode without consuming a prefix.
  pub fn cancel_repeat(&mut self) {
    if matches!(self, Self::Repeat { .. }) {
      *self = Self::Root;
    }
  }

  pub fn resolve(&mut self, key: KeyEvent, prefix: &Prefix, now: Instant) -> Dispatch {
    if key.kind == KeyEventKind::Release {
      return Dispatch::Ignore;
    }
    if let Self::Repeat { until } = *self
      && now >= until
    {
      *self = Self::Root;
    }
    if self.is_prefix() {
      *self = Self::Root;
      if input::matches_prefix(key, prefix) {
        return Dispatch::SendPrefix;
      }
      return actions::resolve(key).map_or(Dispatch::Unknown, |binding| self.command(binding, now));
    }
    if input::matches_prefix(key, prefix) {
      *self = Self::Prefix;
      return Dispatch::Pending;
    }
    if matches!(self, Self::Repeat { .. }) {
      if let Some(binding) = actions::resolve(key).filter(|binding| binding.repeatable) {
        return self.command(binding, now);
      }
      *self = Self::Root;
    }
    Dispatch::Forward
  }

  fn command(&mut self, binding: Binding, now: Instant) -> Dispatch {
    *self = if binding.repeatable {
      Self::Repeat {
        until: now + REPEAT_TIME,
      }
    } else {
      Self::Root
    };
    Dispatch::Action(binding)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::actions::{Action, Direction};
  use crossterm::event::{KeyCode, KeyModifiers};

  fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
  }

  #[test]
  fn repeat_deadline_rearms_for_different_arrows_and_expires_at_the_boundary() {
    let prefix = input::parse_prefix("Ctrl+b").unwrap();
    let now = Instant::now();
    let mut state = KeyState::Root;
    assert_eq!(state.resolve(prefix.key, &prefix, now), Dispatch::Pending);
    assert!(state.is_prefix());
    let first = Binding {
      action: Action::Focus(Direction::Right),
      repeatable: true,
    };
    assert_eq!(
      state.resolve(key(KeyCode::Right), &prefix, now),
      Dispatch::Action(first)
    );
    let later = now + Duration::from_millis(499);
    let second = Binding {
      action: Action::Focus(Direction::Left),
      repeatable: true,
    };
    assert_eq!(
      state.resolve(key(KeyCode::Left), &prefix, later),
      Dispatch::Action(second)
    );
    assert_eq!(
      state,
      KeyState::Repeat {
        until: later + REPEAT_TIME
      }
    );
    assert_eq!(
      state.resolve(key(KeyCode::Right), &prefix, later + REPEAT_TIME),
      Dispatch::Forward
    );
    assert_eq!(state, KeyState::Root);
  }

  #[test]
  fn nonrepeatable_bindings_and_modified_arrows_return_to_root_input() {
    let prefix = input::parse_prefix("Ctrl+b").unwrap();
    let now = Instant::now();
    for key in [
      key(KeyCode::Char('d')),
      key(KeyCode::Char('o')),
      key(KeyCode::Char('c')),
      KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
      KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
    ] {
      let mut state = KeyState::Repeat {
        until: now + REPEAT_TIME,
      };
      assert_eq!(state.resolve(key, &prefix, now), Dispatch::Forward);
      assert_eq!(state, KeyState::Root);
    }
    let mut state = KeyState::Prefix;
    assert_eq!(
      state.resolve(
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        &prefix,
        now
      ),
      Dispatch::Unknown
    );
    assert_eq!(state, KeyState::Root);
  }

  #[test]
  fn reprefix_requires_a_fresh_command_and_double_prefix_is_explicit_input() {
    let prefix = input::parse_prefix("Alt+a").unwrap();
    let now = Instant::now();
    let mut state = KeyState::Repeat {
      until: now + REPEAT_TIME,
    };
    assert_eq!(state.resolve(prefix.key, &prefix, now), Dispatch::Pending);
    assert_eq!(
      state.resolve(prefix.key, &prefix, now),
      Dispatch::SendPrefix
    );
    assert_eq!(state, KeyState::Root);
    assert_eq!(state.resolve(prefix.key, &prefix, now), Dispatch::Pending);
    assert_eq!(
      state.resolve(key(KeyCode::Char('d')), &prefix, now),
      Dispatch::Action(Binding {
        action: Action::Detach,
        repeatable: false
      })
    );
  }

  #[test]
  fn releases_do_not_consume_the_prefix_or_extend_repetition() {
    let prefix = input::parse_prefix("Ctrl+b").unwrap();
    let now = Instant::now();
    let mut release = key(KeyCode::Right);
    release.kind = KeyEventKind::Release;
    for initial in [
      KeyState::Root,
      KeyState::Prefix,
      KeyState::Repeat {
        until: now + REPEAT_TIME,
      },
    ] {
      let mut state = initial;
      assert_eq!(state.resolve(release, &prefix, now), Dispatch::Ignore);
      assert_eq!(state, initial);
    }
    let mut state = KeyState::Prefix;
    let mut repeated = key(KeyCode::Right);
    repeated.kind = KeyEventKind::Repeat;
    assert!(matches!(
      state.resolve(repeated, &prefix, now),
      Dispatch::Action(_)
    ));
  }

  #[test]
  fn leaving_repeat_preserves_an_explicit_pending_prefix() {
    let now = Instant::now();
    let mut state = KeyState::Repeat {
      until: now + REPEAT_TIME,
    };
    state.cancel_repeat();
    assert_eq!(state, KeyState::Root);
    state = KeyState::Prefix;
    state.cancel_repeat();
    assert!(state.is_prefix());
  }
}
