use std::{
  collections::{BTreeSet, HashMap},
  error::Error,
  io::{self, BufRead, Write},
  os::fd::AsFd,
  time::Instant,
};
use wayland_client::{
  Connection, Dispatch, QueueHandle, delegate_noop,
  globals::{GlobalListContents, registry_queue_init},
  protocol::{wl_pointer, wl_registry, wl_seat},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
  zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1 as KeyboardManager,
  zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1 as Keyboard,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
  zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 as PointerManager,
  zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1 as Pointer,
};
use xkbcommon::xkb;

struct Client;
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Client {
  fn event(
    _: &mut Self,
    _: &wl_registry::WlRegistry,
    _: wl_registry::Event,
    _: &GlobalListContents,
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
  }
}
delegate_noop!(Client: ignore wl_seat::WlSeat);
delegate_noop!(Client: ignore KeyboardManager);
delegate_noop!(Client: ignore PointerManager);
delegate_noop!(Client: ignore Keyboard);
delegate_noop!(Client: ignore Pointer);

struct Devices {
  pointer: Pointer,
  keyboard: Keyboard,
  keycodes: HashMap<xkb::Keysym, xkb::Keycode>,
  state: xkb::State,
  keys: BTreeSet<xkb::Keycode>,
  buttons: BTreeSet<u32>,
  start: Instant,
}

impl Devices {
  fn key(&mut self, code: xkb::Keycode, pressed: bool) {
    if !(if pressed {
      self.keys.insert(code)
    } else {
      self.keys.remove(&code)
    }) {
      return;
    }
    self
      .keyboard
      .key(self.time(), code.raw() - 8, u32::from(pressed));
    self.state.update_key(
      code,
      if pressed {
        xkb::KeyDirection::Down
      } else {
        xkb::KeyDirection::Up
      },
    );
    self.keyboard.modifiers(
      self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
      self.state.serialize_mods(xkb::STATE_MODS_LATCHED),
      self.state.serialize_mods(xkb::STATE_MODS_LOCKED),
      self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
    );
  }

  fn time(&self) -> u32 {
    self.start.elapsed().as_millis() as u32
  }

  fn button(&mut self, code: u32, pressed: bool) {
    if !(if pressed {
      self.buttons.insert(code)
    } else {
      self.buttons.remove(&code)
    }) {
      return;
    }
    self.pointer.button(
      self.time(),
      code,
      if pressed {
        wl_pointer::ButtonState::Pressed
      } else {
        wl_pointer::ButtonState::Released
      },
    );
    self.pointer.frame();
  }

  fn reset(&mut self) {
    for code in self.keys.clone() {
      self.key(code, false);
    }
    for code in self.buttons.clone() {
      self.button(code, false);
    }
  }

  fn event(&mut self, line: &str, synchronized: bool) -> Result<(), Box<dyn Error>> {
    let fields: Vec<_> = line.split_ascii_whitespace().collect();
    match fields.as_slice() {
      ["reset"] => self.reset(),
      ["move", x, y, w, h] => {
        let (x, y, w, h) = (x.parse()?, y.parse()?, w.parse()?, h.parse()?);
        if x >= w || y >= h {
          return Err("position outside frame".into());
        }
        self.pointer.motion_absolute(self.time(), x, y, w, h);
        self.pointer.frame();
      }
      ["key", name, state @ ("0" | "1")] => {
        let symbol = xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS);
        match self.keycodes.get(&symbol) {
          Some(&code) if !synchronized || symbol.raw() != 0 => self.key(code, *state == "1"),
          _ if synchronized => return Err("unsupported Wayland key".into()),
          _ => {}
        }
      }
      ["button", button, state @ ("0" | "1")] => {
        let pressed = *state == "1";
        match *button {
          "1" => self.button(0x110, pressed),
          "2" => self.button(0x112, pressed),
          "3" => self.button(0x111, pressed),
          "8" => self.button(0x113, pressed),
          "9" => self.button(0x114, pressed),
          "4" | "5" | "6" | "7" if pressed => {
            let axis = if matches!(*button, "4" | "5") {
              wl_pointer::Axis::VerticalScroll
            } else {
              wl_pointer::Axis::HorizontalScroll
            };
            let steps = if matches!(*button, "4" | "6") { -1 } else { 1 };
            self.pointer.axis_source(wl_pointer::AxisSource::Wheel);
            self
              .pointer
              .axis_discrete(self.time(), axis, 15.0 * f64::from(steps), steps);
            self.pointer.frame();
          }
          "4" | "5" | "6" | "7" => {}
          _ => return Err("unknown button".into()),
        }
      }
      _ => return Err("unknown input event".into()),
    }
    Ok(())
  }
}

pub(super) fn run() -> Result<(), Box<dyn Error>> {
  let synchronized = std::env::args().skip(1).eq(["--sync"]);
  let conn = Connection::connect_to_env()?;
  let (globals, mut queue) = registry_queue_init::<Client>(&conn)?;
  let qh = queue.handle();
  if globals
    .contents()
    .with_list(|items| items.iter().filter(|g| g.interface == "wl_seat").count())
    != 1
  {
    return Err("Wayland control requires one unambiguous seat".into());
  }
  let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ())?;
  let pm: PointerManager = globals.bind(&qh, 1..=1, ())?;
  let km: KeyboardManager = globals.bind(&qh, 1..=1, ())?;
  let pointer = pm.create_virtual_pointer(Some(&seat), &qh, ());
  let keyboard = km.create_virtual_keyboard(&seat, &qh, ());
  let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
  let keymap = xkb::Keymap::new_from_names(
    &context,
    "evdev",
    "pc105",
    "us",
    "",
    None,
    xkb::KEYMAP_COMPILE_NO_FLAGS,
  )
  .ok_or("could not compile input keymap")?;
  let mut text = keymap
    .get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
    .into_bytes();
  text.push(0);
  let mut file = tempfile::tempfile()?;
  file.write_all(&text)?;
  keyboard.keymap(1, file.as_fd(), text.len().try_into()?);
  let mut keycodes = HashMap::new();
  keymap.key_for_each(|map, code| {
    for &symbol in map.key_get_syms_by_level(code, 0, 0) {
      keycodes.entry(symbol).or_insert(code);
    }
  });
  let mut devices = Devices {
    keyboard,
    pointer,
    state: xkb::State::new(&keymap),
    keycodes,
    keys: BTreeSet::new(),
    buttons: BTreeSet::new(),
    start: Instant::now(),
  };
  queue.roundtrip(&mut Client)?;
  println!("ready");
  let result = (|| {
    for line in io::stdin().lock().lines() {
      let line = line?;
      if synchronized && line == "sync" {
        queue.roundtrip(&mut Client)?;
        let mut output = io::stdout().lock();
        output.write_all(b"ok\n")?;
        output.flush()?;
        continue;
      }
      if let Err(error) = devices.event(&line, synchronized) {
        // Validation rejected this command before queuing a Wayland request.
        if synchronized && std::env::var("CTREAM_INPUT_WORKER").as_deref() == Ok("1") {
          let mut output = io::stdout().lock();
          output.write_all(b"refused\n")?;
          output.flush()?;
        }
        return Err(error);
      }
      conn.flush()?;
      queue.dispatch_pending(&mut Client)?;
    }
    Ok(())
  })();
  devices.reset();
  devices.pointer.destroy();
  devices.keyboard.destroy();
  queue.roundtrip(&mut Client)?;
  result
}
