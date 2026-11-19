use ctream::transport::{Channel, Request, Transport};
use eframe::egui::{self, Event, Key, Modifiers, Pos2, Rect, Vec2};
use std::{
  fmt::Write as _,
  io::{self, BufRead, BufReader, Read, Write},
};

pub struct Input {
  channel: Option<Channel>,
  size: Option<[u32; 2]>,
  modifiers: Modifiers,
  pointer: Option<Pos2>,
  scroll: Vec2,
}

pub fn image_rect(area: Rect, [width, height]: [u32; 2]) -> Rect {
  let pixels = Vec2::new(width as f32, height as f32);
  let scale = (area.width() / pixels.x).min(area.height() / pixels.y);
  Rect::from_center_size(area.center(), pixels * scale)
}

fn pixel(rect: Rect, pos: Pos2, [width, height]: [u32; 2]) -> Option<(u32, u32)> {
  if !rect.is_positive() || !rect.contains(pos) {
    return None;
  }
  let axis = |point: f32, min: f32, max: f32, count: u32| {
    let offset = f64::from(point) - f64::from(min);
    let span = f64::from(max) - f64::from(min);
    ((offset * f64::from(count) / span).round() as u32).min(count - 1)
  };
  Some((
    axis(pos.x, rect.min.x, rect.max.x, width),
    axis(pos.y, rect.min.y, rect.max.y, height),
  ))
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

impl Input {
  pub fn connect(transport: &dyn Transport, target: super::Target) -> io::Result<Self> {
    let mut channel = transport.open(Request::Input(target))?;
    let mut header = String::new();
    BufReader::new(channel.reader.by_ref())
      .take(128)
      .read_line(&mut header)?;
    if !header.ends_with('\n') {
      return Err(io::Error::other("incomplete remote input handshake"));
    }
    let size = match target {
      super::Target::Wayland(_) if header.trim_end() == "ready" => None,
      super::Target::X11(_) => {
        let dims: Vec<_> = header.split_whitespace().map(str::parse::<u32>).collect();
        match dims.as_slice() {
          [Ok(w), Ok(h)] if *w > 0 && *h > 0 => Some([*w, *h]),
          _ => {
            return Err(io::Error::other("X11 input initialization failed"));
          }
        }
      }
      _ => {
        return Err(io::Error::other(
          "Wayland input unavailable; check the remote helper and compositor protocols",
        ));
      }
    };
    Ok(Self {
      channel: Some(channel),
      size,
      modifiers: Modifiers::NONE,
      pointer: None,
      scroll: Vec2::ZERO,
    })
  }

  pub fn desktop_size(&self) -> Option<[u32; 2]> {
    self.size
  }

  fn pointer_move(&self, out: &mut String, x: u32, y: u32, [w, h]: [u32; 2]) {
    if self.size.is_none() {
      writeln!(out, "move {x} {y} {w} {h}").unwrap();
    } else {
      writeln!(out, "move {x} {y}").unwrap();
    }
  }

  fn sync_modifiers(&mut self, now: Modifiers, out: &mut String) {
    let old = self.modifiers;
    for (before, after, name) in [
      (old.shift, now.shift, "Shift_L"),
      (old.ctrl, now.ctrl, "Control_L"),
      (old.alt, now.alt, "Alt_L"),
      (old.mac_cmd, now.mac_cmd, "Super_L"),
    ] {
      if before != after {
        writeln!(out, "key {name} {}", u8::from(after)).unwrap();
      }
    }
    self.modifiers = now;
  }

  pub fn reset(&mut self) -> io::Result<()> {
    self.modifiers = Modifiers::NONE;
    self.pointer = None;
    self.scroll = Vec2::ZERO;
    self
      .channel
      .as_mut()
      .expect("open input channel")
      .writer
      .write_all(b"reset\n")
  }

  pub fn forward(
    &mut self,
    rect: Rect,
    size: [u32; 2],
    events: &[Event],
    modifiers: Modifiers,
  ) -> io::Result<()> {
    let mut out = String::new();
    for event in events {
      match *event {
        Event::PointerGone => self.pointer = None,
        Event::PointerMoved(pos) => {
          self.pointer = Some(pos);
          if let Some((x, y)) = pixel(rect, pos, size) {
            self.pointer_move(&mut out, x, y, size);
          }
        }
        Event::PointerButton {
          pos,
          button,
          pressed,
          modifiers,
        } => {
          self.pointer = Some(pos);
          let point = pixel(rect, pos, size);
          if pressed && point.is_none() {
            continue;
          }
          self.sync_modifiers(modifiers, &mut out);
          if let Some((x, y)) = point {
            self.pointer_move(&mut out, x, y, size);
          }
          let code = match button {
            egui::PointerButton::Primary => 1,
            egui::PointerButton::Middle => 2,
            egui::PointerButton::Secondary => 3,
            egui::PointerButton::Extra1 => 8,
            egui::PointerButton::Extra2 => 9,
          };
          writeln!(out, "button {code} {}", u8::from(pressed)).unwrap();
        }
        Event::MouseWheel {
          unit,
          delta,
          modifiers,
        } if self.pointer.is_some_and(|p| rect.contains(p)) => {
          self.sync_modifiers(modifiers, &mut out);
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
              writeln!(out, "button {code} 1\nbutton {code} 0").unwrap();
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
          self.sync_modifiers(modifiers, &mut out);
          writeln!(
            out,
            "key {} {}",
            keysym(physical_key.unwrap_or(key)),
            u8::from(pressed)
          )
          .unwrap();
        }
        Event::Copy | Event::Cut | Event::Paste(_) => {
          self.sync_modifiers(modifiers, &mut out);
          let key = match event {
            Event::Copy => "c",
            Event::Cut => "x",
            _ => "v",
          };
          writeln!(out, "key {key} 1\nkey {key} 0").unwrap();
        }
        _ => {}
      }
    }
    self.sync_modifiers(modifiers, &mut out);
    self
      .channel
      .as_mut()
      .expect("open input channel")
      .writer
      .write_all(out.as_bytes())
  }
}

impl Drop for Input {
  fn drop(&mut self) {
    let Channel { mut reader, writer } = self.channel.take().expect("open input channel");
    drop(writer); // EOF makes the remote helper release all held input.
    std::thread::spawn(move || {
      let _ = io::copy(&mut reader, &mut io::sink());
    });
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn resized_image_and_clicks_share_coordinates() {
    for size in [[600., 900.], [1600., 500.], [1280., 720.]] {
      let area = Rect::from_min_size(Pos2::new(8., 16.), size.into());
      let dims = [1280, 720];
      let rect = image_rect(area, dims);
      assert_eq!(pixel(rect, rect.min, dims), Some((0, 0)));
      assert_eq!(pixel(rect, rect.center(), dims), Some((640, 360)));
      assert_eq!(pixel(rect, rect.max, dims), Some((1279, 719)));
      assert_eq!(pixel(rect, rect.min - Vec2::splat(1.), dims), None);
    }
  }
}
