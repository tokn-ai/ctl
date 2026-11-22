use ctream_client::{
  geometry::{ImageGeometry, Point},
  input::Input as RemoteInput,
};
use eframe::egui::{self, Event, Key, Modifiers, Pos2, Vec2};
use std::io::{self, Read, Write};

pub(super) trait Sink {
  fn dimensions(&self) -> Option<[u32; 2]> {
    None
  }

  fn move_pointer(&mut self, x: u32, y: u32, size: [u32; 2]) -> io::Result<()>;
  fn button(&mut self, code: u8, pressed: bool) -> io::Result<()>;
  fn key(&mut self, name: &str, pressed: bool) -> io::Result<()>;
  fn reset(&mut self) -> io::Result<()>;
}

impl<R: Read + Send + 'static, W: Write + Send + 'static> Sink for RemoteInput<R, W> {
  fn dimensions(&self) -> Option<[u32; 2]> {
    Self::dimensions(self)
  }

  fn move_pointer(&mut self, x: u32, y: u32, size: [u32; 2]) -> io::Result<()> {
    Self::move_pointer(self, x, y, size)
  }

  fn button(&mut self, code: u8, pressed: bool) -> io::Result<()> {
    Self::button(self, code, pressed)
  }

  fn key(&mut self, name: &str, pressed: bool) -> io::Result<()> {
    Self::key(self, name, pressed)
  }

  fn reset(&mut self) -> io::Result<()> {
    Self::reset(self)
  }
}

pub(super) struct Input<S: Sink> {
  control: S,
  forwarding: bool,
  modifiers: Modifiers,
  pointer: Option<Pos2>,
  buttons: [bool; egui::NUM_POINTER_BUTTONS],
  scroll: Vec2,
}

fn pixel(image: &ImageGeometry, pos: Pos2) -> Option<[u32; 2]> {
  image.pixel(Point::new(f64::from(pos.x), f64::from(pos.y)))
}

fn is_local(pos: Pos2, regions: &[egui::Rect]) -> bool {
  regions.iter().any(|region| region.contains(pos))
}

fn keysym(key: Key) -> String {
  match key {
    Key::Enter => "Return",
    Key::Backspace => "BackSpace",
    Key::PageUp => "Prior",
    Key::PageDown => "Next",
    Key::Equals => "equal",
    Key::Backtick => "grave",
    Key::OpenBracket => "bracketleft",
    Key::CloseBracket => "bracketright",
    Key::Quote => "apostrophe",
    Key::Pipe => "bar",
    Key::Questionmark => "question",
    Key::Exclamationmark => "exclam",
    Key::OpenCurlyBracket => "braceleft",
    Key::CloseCurlyBracket => "braceright",
    Key::BrowserBack => "XF86Back",
    Key::Space
    | Key::Backslash
    | Key::Slash
    | Key::Period
    | Key::Comma
    | Key::Semicolon
    | Key::Minus
    | Key::Plus
    | Key::Colon => return key.name().to_lowercase(),
    _ if key.name().len() == 1 => return key.name().to_lowercase(),
    _ => key.name(),
  }
  .into()
}

impl<S: Sink> Input<S> {
  pub fn new(control: S) -> Self {
    Self {
      control,
      forwarding: false,
      modifiers: Modifiers::NONE,
      pointer: None,
      buttons: [false; egui::NUM_POINTER_BUTTONS],
      scroll: Vec2::ZERO,
    }
  }

  pub fn desktop_size(&self) -> Option<[u32; 2]> {
    self.control.dimensions()
  }

  #[cfg(test)]
  pub fn is_forwarding(&self) -> bool {
    self.forwarding
  }

  pub fn update(
    &mut self,
    ctx: &egui::Context,
    image: Option<&ImageGeometry>,
    pointer_regions: &[egui::Rect],
  ) -> io::Result<()> {
    if ctx.current_pass_index() != 0 {
      return Ok(());
    }
    if let Some(image) = image
      && ctx.input(|input| input.focused)
      && !ctx.egui_wants_keyboard_input()
    {
      self.forwarding = true;
      let (events, modifiers) = ctx.input(|input| (input.raw.events.clone(), input.modifiers));
      return self.forward(image, &events, modifiers, pointer_regions);
    }
    if std::mem::take(&mut self.forwarding) {
      self.reset()?;
    }
    Ok(())
  }

  fn sync_modifiers(&mut self, now: Modifiers) -> io::Result<()> {
    let old = self.modifiers;
    for (before, after, name) in [
      (old.shift, now.shift, "Shift_L"),
      (old.ctrl, now.ctrl, "Control_L"),
      (old.alt, now.alt, "Alt_L"),
      (old.mac_cmd, now.mac_cmd, "Super_L"),
    ] {
      if before != after {
        self.control.key(name, after)?;
      }
    }
    self.modifiers = now;
    Ok(())
  }

  fn reset(&mut self) -> io::Result<()> {
    self.modifiers = Modifiers::NONE;
    self.pointer = None;
    self.buttons.fill(false);
    self.scroll = Vec2::ZERO;
    self.control.reset()
  }

  fn forward_button(
    &mut self,
    image: &ImageGeometry,
    pos: Pos2,
    button: egui::PointerButton,
    pressed: bool,
    modifiers: Modifiers,
    pointer_regions: &[egui::Rect],
  ) -> io::Result<()> {
    self.pointer = Some(pos);
    let point = pixel(image, pos);
    let held = &mut self.buttons[button as usize];
    // The press owns the gesture. Always release an already forwarded button,
    // even if the pointer is now over the local controls or outside the image.
    let forward = if pressed {
      point.is_some() && !is_local(pos, pointer_regions)
    } else {
      *held
    };
    if !forward {
      return Ok(());
    }
    *held = pressed;
    self.sync_modifiers(modifiers)?;
    if let Some([x, y]) = point {
      self.control.move_pointer(x, y, image.size())?;
    }
    let code = match button {
      egui::PointerButton::Primary => 1,
      egui::PointerButton::Middle => 2,
      egui::PointerButton::Secondary => 3,
      egui::PointerButton::Extra1 => 8,
      egui::PointerButton::Extra2 => 9,
    };
    self.control.button(code, pressed)
  }

  fn forward(
    &mut self,
    image: &ImageGeometry,
    events: &[Event],
    modifiers: Modifiers,
    pointer_regions: &[egui::Rect],
  ) -> io::Result<()> {
    for event in events {
      match *event {
        Event::PointerGone => self.pointer = None,
        Event::PointerMoved(pos) => {
          self.pointer = Some(pos);
          if (!is_local(pos, pointer_regions) || self.buttons.contains(&true))
            && let Some([x, y]) = pixel(image, pos)
          {
            self.control.move_pointer(x, y, image.size())?;
          }
        }
        Event::PointerButton {
          pos,
          button,
          pressed,
          modifiers,
        } => {
          self.forward_button(image, pos, button, pressed, modifiers, pointer_regions)?;
        }
        Event::MouseWheel {
          unit,
          delta,
          modifiers,
          ..
        } if self
          .pointer
          .is_some_and(|pos| !is_local(pos, pointer_regions) && pixel(image, pos).is_some()) =>
        {
          if let Some([x, y]) = self.pointer.and_then(|point| pixel(image, point)) {
            self.control.move_pointer(x, y, image.size())?;
          }
          self.sync_modifiers(modifiers)?;
          self.scroll += delta
            * match unit {
              egui::MouseWheelUnit::Point => 1.0 / 32.0,
              egui::MouseWheelUnit::Line => 1.0,
              egui::MouseWheelUnit::Page => 8.0,
            };
          for (steps, positive, negative) in
            [(&mut self.scroll.y, 4, 5), (&mut self.scroll.x, 6, 7)]
          {
            let code = if *steps > 0.0 { positive } else { negative };
            for _ in 0..(steps.abs() as usize).min(32) {
              self.control.button(code, true)?;
              self.control.button(code, false)?;
            }
            *steps = steps.fract();
          }
        }
        Event::Key {
          key,
          physical_key,
          pressed,
          repeat: false,
          modifiers,
        } => {
          self.sync_modifiers(modifiers)?;
          self
            .control
            .key(&keysym(physical_key.unwrap_or(key)), pressed)?;
        }
        Event::Copy | Event::Cut | Event::Paste(_) => {
          self.sync_modifiers(modifiers)?;
          let key = match event {
            Event::Copy => "c",
            Event::Cut => "x",
            _ => "v",
          };
          self.control.key(key, true)?;
          self.control.key(key, false)?;
        }
        _ => {}
      }
    }
    self.sync_modifiers(modifiers)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn wheel_remaps_stationary_pointer_after_viewport_and_capture_resize() {
    let (session, observed) = crate::test_support::session(b"");
    let mut input = Input::new(session.input().unwrap());
    let geometry = |width, height, frame| {
      crate::view::fit(
        egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(width, height)),
        frame,
      )
      .unwrap()
    };
    input
      .forward(
        &geometry(800.0, 600.0, [800, 400]),
        &[Event::PointerMoved(egui::pos2(200.0, 300.0))],
        Modifiers::NONE,
        &[],
      )
      .unwrap();
    assert_eq!(*observed.bytes.lock().unwrap(), b"move 200 200 800 400\n");

    let wheel = Event::MouseWheel {
      unit: egui::MouseWheelUnit::Line,
      delta: egui::vec2(0.0, 1.0),
      modifiers: Modifiers::NONE,
      phase: egui::TouchPhase::Move,
      source: egui::MouseWheelSource::Wheel,
    };
    for (image, expected) in [
      (
        geometry(400.0, 600.0, [800, 400]),
        "move 400 200 800 400\nbutton 4 1\nbutton 4 0\n",
      ),
      (
        geometry(400.0, 600.0, [400, 800]),
        "move 200 400 400 800\nbutton 4 1\nbutton 4 0\n",
      ),
      (geometry(100.0, 100.0, [400, 800]), ""),
    ] {
      observed.bytes.lock().unwrap().clear();
      // No PointerMoved event arrives between these geometry changes.
      input
        .forward(&image, std::slice::from_ref(&wheel), Modifiers::NONE, &[])
        .unwrap();
      assert_eq!(*observed.bytes.lock().unwrap(), expected.as_bytes());
    }
  }
}
