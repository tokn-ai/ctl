use super::{
  protocol::{Key, frame_position, normalize_position},
  state::Backend,
};
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Capture {
  None,
  Target,
  Other,
}

pub(super) struct Routing {
  pub foreground: bool,
  pub focus: bool,
  pub capture: Capture,
}

pub(super) trait WindowBackend {
  fn validate(&mut self) -> io::Result<()>;
  fn geometry(&mut self) -> io::Result<([i32; 4], [i32; 4])>;
  fn routing(&mut self) -> io::Result<Routing>;
  fn activate(&mut self) -> io::Result<()>;
  fn hit_target(&mut self, physical: [i32; 2]) -> io::Result<bool>;
  fn move_pointer(&mut self, absolute: [i32; 2]) -> io::Result<()>;
  fn wait_pointer(&mut self, physical: [i32; 2]) -> io::Result<()>;
  fn key(&mut self, key: Key, pressed: bool) -> io::Result<()>;
  fn button(&mut self, code: u8, pressed: bool) -> io::Result<()>;
}

/// Window routing wraps the same injection and held-input lifecycle as displays.
/// Suppressed hover still records its position so a later explicit click can
/// activate the window and remap that position against its current bounds.
pub(super) struct Window<B> {
  backend: B,
  pointer: Option<([u32; 2], [u32; 2])>,
}

impl<B: WindowBackend> Window<B> {
  pub fn new(backend: B) -> Self {
    Self {
      backend,
      pointer: None,
    }
  }

  fn point(&mut self, point: [u32; 2], size: [u32; 2]) -> io::Result<([i32; 2], [i32; 2])> {
    let (bounds, desktop) = self.backend.geometry()?;
    let physical = frame_position(point, size, bounds)?;
    Ok((physical, normalize_position(physical, desktop)?))
  }

  fn require_pointer_target(
    &mut self,
    physical: [i32; 2],
    background_click: bool,
  ) -> io::Result<()> {
    let route = self.backend.routing()?;
    let routed = if route.foreground {
      route.capture != Capture::Other
    } else {
      background_click && route.capture == Capture::None
    };
    if !routed || !self.backend.hit_target(physical)? {
      return Err(io::Error::other(
        "selected window is covered or cannot receive pointer input safely; input was not sent",
      ));
    }
    Ok(())
  }
}

impl<B: WindowBackend> Backend for Window<B> {
  fn validate_target(&mut self) -> io::Result<()> {
    self.backend.validate()
  }

  fn move_pointer(&mut self, point: [u32; 2], size: [u32; 2]) -> io::Result<()> {
    let (physical, absolute) = self.point(point, size)?;
    self.pointer = Some((point, size));
    let route = self.backend.routing()?;
    // A covered/background window must not move the real cursor over another
    // application's controls. Captured dragging is allowed only for this root.
    if route.foreground
      && route.capture != Capture::Other
      && (route.capture == Capture::Target || self.backend.hit_target(physical)?)
    {
      self.backend.move_pointer(absolute)?;
    }
    Ok(())
  }

  fn key(&mut self, key: Key, pressed: bool) -> io::Result<()> {
    if pressed {
      let route = self.backend.routing()?;
      if !route.foreground || !route.focus {
        return Err(io::Error::other(
          "selected window does not have keyboard focus; click it before typing",
        ));
      }
      self.backend.validate()?;
    }
    self.backend.key(key, pressed)
  }

  fn button(&mut self, code: u8, pressed: bool) -> io::Result<()> {
    if pressed {
      let (point, size) = self.pointer.ok_or_else(|| {
        io::Error::other("window input requires a pointer position before clicking or scrolling")
      })?;
      let background_click = if !self.backend.routing()?.foreground {
        match self.backend.activate() {
          Ok(()) => false,
          // Windows may deny foreground activation for a background agent.
          // A physical click can use ordinary Windows mouse activation, but
          // only if its point is visible and no foreground window owns capture.
          Err(_) if matches!(code, 1..=3 | 8..=9) => true,
          Err(error) => return Err(error),
        }
      } else {
        false
      };
      let (physical, absolute) = self.point(point, size)?;
      self.require_pointer_target(physical, background_click)?;
      self.backend.validate()?;
      self.backend.move_pointer(absolute)?;
      // SendInput can be clipped or redirected. Verify the actual cursor before
      // sending a press, then recheck routing and identity after the move.
      self.backend.wait_pointer(physical)?;
      self.backend.validate()?;
      let (current, _) = self.point(point, size)?;
      if current != physical {
        return Err(io::Error::other(
          "selected window moved while positioning input; input was not sent",
        ));
      }
      self.require_pointer_target(physical, background_click)?;
    }
    self.backend.button(code, pressed)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::windows::{protocol::key, state};
  use std::{cell::RefCell, rc::Rc};

  struct State {
    foreground: bool,
    focus: bool,
    covered: bool,
    capture: Capture,
    capture_after_move: Option<Capture>,
    allow_activation: bool,
    valid: bool,
    invalidate_on_activation: bool,
    bounds: [i32; 4],
    bounds_after_wait: Option<[i32; 4]>,
    cursor_reached: bool,
    events: Vec<&'static str>,
    positions: Vec<[i32; 2]>,
  }

  struct Fake(Rc<RefCell<State>>);
  impl WindowBackend for Fake {
    fn validate(&mut self) -> io::Result<()> {
      if self.0.borrow().valid {
        Ok(())
      } else {
        Err(io::Error::other("selected window was destroyed"))
      }
    }
    fn geometry(&mut self) -> io::Result<([i32; 4], [i32; 4])> {
      Ok((self.0.borrow().bounds, [-1000, 0, 1000, 1000]))
    }
    fn routing(&mut self) -> io::Result<Routing> {
      let state = self.0.borrow();
      Ok(Routing {
        foreground: state.foreground,
        focus: state.focus,
        capture: state.capture,
      })
    }
    fn activate(&mut self) -> io::Result<()> {
      let mut state = self.0.borrow_mut();
      state.events.push("activate");
      if state.invalidate_on_activation {
        state.valid = false;
      }
      if state.allow_activation {
        state.foreground = true;
        state.focus = true;
      } else {
        return Err(io::Error::other("foreground activation denied"));
      }
      Ok(())
    }
    fn hit_target(&mut self, _: [i32; 2]) -> io::Result<bool> {
      Ok(!self.0.borrow().covered)
    }
    fn move_pointer(&mut self, position: [i32; 2]) -> io::Result<()> {
      let mut state = self.0.borrow_mut();
      state.events.push("move");
      state.positions.push(position);
      if let Some(capture) = state.capture_after_move {
        state.capture = capture;
      }
      Ok(())
    }
    fn wait_pointer(&mut self, _: [i32; 2]) -> io::Result<()> {
      let mut state = self.0.borrow_mut();
      if !state.cursor_reached {
        return Err(io::Error::other("cursor clipped"));
      }
      if let Some(bounds) = state.bounds_after_wait {
        state.bounds = bounds;
      }
      Ok(())
    }
    fn key(&mut self, _: Key, pressed: bool) -> io::Result<()> {
      self
        .0
        .borrow_mut()
        .events
        .push(if pressed { "key_down" } else { "key_up" });
      Ok(())
    }
    fn button(&mut self, _: u8, pressed: bool) -> io::Result<()> {
      self
        .0
        .borrow_mut()
        .events
        .push(if pressed { "button_down" } else { "button_up" });
      Ok(())
    }
  }

  fn setup() -> (Window<Fake>, Rc<RefCell<State>>) {
    let state = Rc::new(RefCell::new(State {
      foreground: false,
      focus: false,
      covered: false,
      capture: Capture::None,
      capture_after_move: None,
      allow_activation: true,
      valid: true,
      invalidate_on_activation: false,
      bounds: [-800, 100, -400, 300],
      bounds_after_wait: None,
      cursor_reached: true,
      events: Vec::new(),
      positions: Vec::new(),
    }));
    (Window::new(Fake(state.clone())), state)
  }

  #[test]
  fn hover_never_activates_and_explicit_click_tracks_resized_moved_window() {
    let (mut window, state) = setup();
    window.move_pointer([50, 25], [100, 50]).unwrap();
    assert!(state.borrow().events.is_empty());
    state.borrow_mut().bounds = [100, 200, 900, 600];
    window.button(1, true).unwrap();
    assert_eq!(state.borrow().events, ["activate", "move", "button_down"]);
    let physical = frame_position([50, 25], [100, 50], [100, 200, 900, 600]).unwrap();
    assert_eq!(physical, [504, 404]);
    assert_eq!(
      state.borrow().positions,
      [normalize_position(physical, [-1000, 0, 1000, 1000]).unwrap()]
    );
  }

  #[test]
  fn denied_activation_can_use_normal_mouse_activation_only_for_visible_physical_clicks() {
    for code in [1, 2, 3, 8, 9] {
      let (mut window, state) = setup();
      state.borrow_mut().allow_activation = false;
      window.move_pointer([10, 10], [100, 50]).unwrap();
      assert!(state.borrow().events.is_empty());
      window.button(code, true).unwrap();
      assert_eq!(state.borrow().events, ["activate", "move", "button_down"]);
      // A window may eat the activation click. Keyboard input must still wait
      // for actual foreground focus instead of assuming the press activated it.
      assert!(window.key(key("F8").unwrap(), true).is_err());
      window.button(code, false).unwrap();
      assert_eq!(state.borrow().events.last(), Some(&"button_up"));
    }
    for code in 4..=7 {
      let (mut window, state) = setup();
      state.borrow_mut().allow_activation = false;
      window.move_pointer([10, 10], [100, 50]).unwrap();
      assert!(window.button(code, true).is_err());
      assert_eq!(state.borrow().events, ["activate"]);
    }
  }

  #[test]
  fn covered_captured_or_clipped_background_targets_never_receive_presses() {
    for failure in 0..6 {
      let (mut window, state) = setup();
      state.borrow_mut().allow_activation = false;
      window.move_pointer([10, 10], [100, 50]).unwrap();
      match failure {
        0 => state.borrow_mut().covered = true,
        1 => state.borrow_mut().capture = Capture::Other,
        2 => state.borrow_mut().cursor_reached = false,
        3 => state.borrow_mut().capture_after_move = Some(Capture::Other),
        4 => state.borrow_mut().invalidate_on_activation = true,
        _ => state.borrow_mut().bounds_after_wait = Some([-790, 100, -390, 300]),
      }
      assert!(window.button(1, true).is_err());
      assert!(!state.borrow().events.contains(&"button_down"));
      if failure == 4 {
        assert_eq!(state.borrow().events, ["activate"]);
      }
    }
  }

  #[test]
  fn keyboard_requires_focus_without_activation_and_releases_bypass_routing() {
    let (mut window, state) = setup();
    let key = key("F8").unwrap();
    assert!(window.key(key, true).is_err());
    state.borrow_mut().foreground = true;
    assert!(window.key(key, true).is_err());
    assert!(state.borrow().events.is_empty());
    state.borrow_mut().focus = true;
    window.key(key, true).unwrap();
    state.borrow_mut().foreground = false;
    window.key(key, false).unwrap();
    window.button(1, false).unwrap();
    assert_eq!(state.borrow().events, ["key_down", "key_up", "button_up"]);
  }

  #[test]
  fn only_the_selected_roots_capture_permits_covered_dragging() {
    for capture in [Capture::None, Capture::Target, Capture::Other] {
      let (mut window, state) = setup();
      state.borrow_mut().foreground = true;
      state.borrow_mut().covered = true;
      state.borrow_mut().capture = capture;
      window.move_pointer([10, 10], [100, 50]).unwrap();
      assert_eq!(
        state.borrow().events.contains(&"move"),
        capture == Capture::Target
      );
    }
  }

  #[test]
  fn routing_failure_releases_previously_injected_inputs() {
    let (window, state) = setup();
    state.borrow_mut().foreground = true;
    state.borrow_mut().focus = true;
    assert!(
      state::run(
        false,
        window,
        &mut &b"key F8 1\nbutton 1 1\n"[..],
        &mut Vec::new(),
      )
      .is_err()
    );
    assert_eq!(state.borrow().events, ["key_down", "key_up"]);
  }
}
