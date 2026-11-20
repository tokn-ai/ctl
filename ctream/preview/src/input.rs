use ctream_client::{
  geometry::{ImageGeometry, Point},
  input::Input as RemoteInput,
};
use eframe::egui::{self, Event, Key, Modifiers, Pos2, Vec2};
use std::io::{self, Read, Write};

pub struct Input<R: Read + Send + 'static, W: Write + Send + 'static> {
  control: RemoteInput<R, W>,
  modifiers: Modifiers,
  pointer: Option<Pos2>,
  scroll: Vec2,
}

fn pixel(image: &ImageGeometry, pos: Pos2) -> Option<[u32; 2]> {
  image.pixel(Point::new(f64::from(pos.x), f64::from(pos.y)))
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

impl<R: Read + Send + 'static, W: Write + Send + 'static> Input<R, W> {
  pub fn new(control: RemoteInput<R, W>) -> Self {
    Self {
      control,
      modifiers: Modifiers::NONE,
      pointer: None,
      scroll: Vec2::ZERO,
    }
  }

  pub fn desktop_size(&self) -> Option<[u32; 2]> {
    self.control.dimensions()
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

  pub fn reset(&mut self) -> io::Result<()> {
    self.modifiers = Modifiers::NONE;
    self.pointer = None;
    self.scroll = Vec2::ZERO;
    self.control.reset()
  }

  pub fn forward(
    &mut self,
    image: &ImageGeometry,
    events: &[Event],
    modifiers: Modifiers,
  ) -> io::Result<()> {
    for event in events {
      match *event {
        Event::PointerGone => self.pointer = None,
        Event::PointerMoved(pos) => {
          self.pointer = Some(pos);
          if let Some([x, y]) = pixel(image, pos) {
            self.control.move_pointer(x, y, image.size())?;
          }
        }
        Event::PointerButton {
          pos,
          button,
          pressed,
          modifiers,
        } => {
          self.pointer = Some(pos);
          let point = pixel(image, pos);
          if pressed && point.is_none() {
            continue;
          }
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
          self.control.button(code, pressed)?;
        }
        Event::MouseWheel {
          unit,
          delta,
          modifiers,
        } if self
          .pointer
          .is_some_and(|point| pixel(image, point).is_some()) =>
        {
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
