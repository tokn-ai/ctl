use ctream_driver::{InputSource, Result};
use ctream_proto::WindowId;
use std::{
  collections::BTreeMap,
  io::{self, BufRead, Read, Write},
  time::Instant,
};
use x11rb::{
  connection::Connection,
  protocol::{
    Event as XEvent,
    xproto::{
      AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ButtonPressEvent,
      ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask, GetGeometryReply,
      KEY_PRESS_EVENT, KEY_RELEASE_EVENT, KeyButMask, KeyPressEvent, MOTION_NOTIFY_EVENT, MapState,
      Motion, MotionNotifyEvent, PropMode, Timestamp, Window, WindowClass,
    },
  },
  rust_connection::RustConnection,
};

const MAX_WINDOW_DEPTH: usize = 64;

pub struct WindowInput {
  conn: RustConnection,
  root: Window,
  window: Window,
  keyboard: Keyboard,
  clock: (Timestamp, Instant),
  closed: bool,
  pointer: Option<FramePoint>,
  keyboard_point: Option<FramePoint>,
  keys: BTreeMap<u8, Window>,
  buttons: BTreeMap<u8, Window>,
  locks: u16,
}

impl WindowInput {
  pub(super) fn open(display: &str, id: &str) -> Result<Self> {
    let window = WindowId::x11_xid(id).ok_or("invalid X11 window ID")?;
    let (conn, screen) = x11rb::connect(Some(display))?;
    let root = conn.setup().roots[screen].root;
    let client_list = conn.intern_atom(true, b"_NET_CLIENT_LIST")?.reply()?.atom;
    let clients = if client_list == 0 {
      None
    } else {
      conn
        .get_property(false, root, client_list, AtomEnum::WINDOW, 0, 16384)?
        .reply()?
        .value32()
        .map(|ids| ids.collect::<Vec<_>>())
    };
    let clients = match clients {
      Some(clients) => clients,
      None => conn.query_tree(root)?.reply()?.children,
    };
    if !clients.contains(&window) {
      return Err("X11 input target is not a top-level client window".into());
    }

    let clock = server_clock(&conn, root)?;
    conn
      .change_window_attributes(
        window,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
      )?
      .check()?;
    let keyboard = Keyboard::read(&conn)?;
    let mut input = Self {
      conn,
      root,
      window,
      keyboard,
      clock,
      closed: false,
      pointer: None,
      keyboard_point: None,
      keys: BTreeMap::new(),
      buttons: BTreeMap::new(),
      locks: 0,
    };
    input.geometry()?;
    Ok(input)
  }

  fn geometry(&mut self) -> Result<GetGeometryReply> {
    if self.closed {
      return Err("X11 input target closed".into());
    }
    let attrs = self.conn.get_window_attributes(self.window)?;
    let geometry = self.conn.get_geometry(self.window)?;
    let attrs = attrs.reply()?;
    let geometry = geometry.reply()?;
    while let Some(event) = self.conn.poll_for_event()? {
      if matches!(event, XEvent::DestroyNotify(event) if event.window == self.window) {
        self.closed = true;
        return Err("X11 input target closed".into());
      }
    }
    if geometry.root != self.root
      || attrs.class != WindowClass::INPUT_OUTPUT
      || attrs.override_redirect
      || attrs.map_state != MapState::VIEWABLE
    {
      return Err("X11 input target is no longer viewable".into());
    }
    Ok(geometry)
  }

  fn dispatch(&mut self, command: Command<'_>, synchronized: bool) -> Result<()> {
    let geometry = self.geometry()?;
    let size = [geometry.width, geometry.height];

    match command {
      Command::Move(pointer) => {
        self.pointer = Some(pointer);
        let point = pointer.in_window(size)?;
        let window = match self.buttons.values().next() {
          Some(&window) => window,
          None => self.route(point, EventMask::POINTER_MOTION)?,
        };
        self.send(window, point, InputEvent::Motion)?;
      }
      Command::Button(code, pressed) => {
        if pressed && !self.buttons.contains_key(&code) {
          let pointer = self
            .pointer
            .ok_or("X11 window input requires a pointer position")?;
          let point = pointer.in_window(size)?;
          let window = match self.buttons.values().next() {
            Some(&window) => window,
            None => self.route(point, EventMask::BUTTON_PRESS)?,
          };
          self.send(window, point, InputEvent::Button(code, true))?;
          self.buttons.insert(code, window);
          if matches!(code, 1..=3) {
            self.keyboard_point = Some(pointer);
          }
        } else if !pressed {
          self.release_button(code, self.point(size)?)?;
        }
      }
      Command::Key(name, pressed) => {
        // Some layouts do not have all keys the preview can name.
        let Some(&code) = self.keyboard.codes.get(canonical_key(name)) else {
          if synchronized {
            return Err("unsupported X11 key".into());
          }
          return Ok(());
        };
        let point = self.point(size)?;
        if pressed && !self.keys.contains_key(&code) {
          let window = match self.keyboard_point {
            Some(pointer) => self.route(pointer.in_window(size)?, EventMask::KEY_PRESS)?,
            None => self.window,
          };
          self.send(window, point, InputEvent::Key(code, true))?;
          self.keys.insert(code, window);
          if matches!(
            canonical_key(name),
            "Caps_Lock" | "Num_Lock" | "Scroll_Lock"
          ) {
            self.locks ^= self.keyboard.modifier(code);
          }
        } else if !pressed {
          self.release_key(code, point)?;
        }
      }
      Command::Reset => {
        self.release_all(size)?;
        self.keyboard_point = None;
        self.pointer = None;
      }
    }
    Ok(())
  }

  fn point(&self, size: [u16; 2]) -> Result<[i16; 2]> {
    self
      .pointer
      .map_or(Ok([0, 0]), |pointer| pointer.in_window(size))
  }

  fn release_all(&mut self, size: [u16; 2]) -> Result<()> {
    let point = self.point(size)?;
    let mut result = Ok(());
    for code in self.buttons.keys().copied().collect::<Vec<_>>() {
      result = result.and(self.release_button(code, point));
    }
    for code in self.keys.keys().copied().collect::<Vec<_>>() {
      result = result.and(self.release_key(code, point));
    }
    self.locks = 0;
    result
  }

  fn release_button(&mut self, code: u8, point: [i16; 2]) -> Result<()> {
    let result = match self.buttons.get(&code) {
      Some(&window) => self.send(window, point, InputEvent::Button(code, false)),
      None => Ok(()),
    };
    self.buttons.remove(&code);
    result
  }

  fn release_key(&mut self, code: u8, point: [i16; 2]) -> Result<()> {
    let result = match self.keys.get(&code) {
      Some(&window) => self.send(window, point, InputEvent::Key(code, false)),
      None => Ok(()),
    };
    self.keys.remove(&code);
    result
  }

  fn button_state(&self) -> u16 {
    self
      .buttons
      .keys()
      .filter(|code| (1..=5).contains(*code))
      .fold(0, |state, code| state | (1 << (7 + code)))
  }

  /// Find the deepest event-selecting child at the synthetic pointer, starting
  /// inside the client hierarchy so covering or focusing another window has no effect.
  fn route(&self, [x, y]: [i16; 2], mask: EventMask) -> Result<Window> {
    let mut window = self.window;
    let mut destination = self.window;
    for _ in 0..MAX_WINDOW_DEPTH {
      let reply = self
        .conn
        .translate_coordinates(self.window, window, x, y)?
        .reply()?;
      if reply.child == 0 {
        return Ok(destination);
      }
      window = reply.child;
      let attrs = self.conn.get_window_attributes(window)?.reply()?;
      if mask_matches(attrs.all_event_masks, mask) {
        destination = window;
      }
    }
    Err("X11 input child hierarchy is too deep".into())
  }

  fn send(&self, window: Window, [x, y]: [i16; 2], event: InputEvent) -> Result<()> {
    // A child retained from a held button or key may have been reparented.
    let mut ancestor = window;
    for _ in 0..MAX_WINDOW_DEPTH {
      if ancestor == self.window {
        break;
      }
      ancestor = self.conn.query_tree(ancestor)?.reply()?.parent;
      if ancestor == 0 || ancestor == self.root {
        break;
      }
    }
    if ancestor != self.window {
      return Err("X11 input receiver left the selected client window".into());
    }

    let root = self
      .conn
      .translate_coordinates(self.window, self.root, x, y)?;
    let local = self.conn.translate_coordinates(self.window, window, x, y)?;
    let attrs = self.conn.get_window_attributes(window)?;
    let root = root.reply()?;
    let local = local.reply()?;
    if attrs.reply()?.map_state != MapState::VIEWABLE || !root.same_screen || !local.same_screen {
      return Err("X11 input receiver is no longer viewable".into());
    }

    let state = self
      .keys
      .keys()
      .fold(self.locks | self.button_state(), |state, code| {
        state | self.keyboard.modifier(*code) & !self.keyboard.lock_mask
      });
    let time = self
      .clock
      .0
      .wrapping_add(self.clock.1.elapsed().as_millis() as u32);
    // Address only the window owner. Other clients may have selected input
    // on the window and must not see the synthetic event.
    macro_rules! send_event {
      ($class:ident, $response:expr, $detail:expr) => {
        self
          .conn
          .send_event(
            false,
            window,
            EventMask::NO_EVENT,
            $class {
              response_type: $response,
              detail: $detail,
              sequence: 0,
              time,
              root: self.root,
              event: window,
              child: local.child,
              root_x: root.dst_x,
              root_y: root.dst_y,
              event_x: local.dst_x,
              event_y: local.dst_y,
              state: KeyButMask::from(state),
              same_screen: true,
            },
          )?
          .check()?
      };
    }
    match event {
      InputEvent::Motion => send_event!(MotionNotifyEvent, MOTION_NOTIFY_EVENT, Motion::NORMAL),
      InputEvent::Button(code, pressed) => send_event!(
        ButtonPressEvent,
        if pressed {
          BUTTON_PRESS_EVENT
        } else {
          BUTTON_RELEASE_EVENT
        },
        code
      ),
      InputEvent::Key(code, pressed) => send_event!(
        KeyPressEvent,
        if pressed {
          KEY_PRESS_EVENT
        } else {
          KEY_RELEASE_EVENT
        },
        code
      ),
    }
    Ok(())
  }

  fn receive(
    &mut self,
    reader: &mut impl BufRead,
    output: &mut impl Write,
    synchronized: bool,
  ) -> Result<()> {
    let mut line = String::new();
    loop {
      line.clear();
      let bytes = reader.take(129).read_line(&mut line)?;
      if bytes == 0 {
        return Ok(());
      }
      if bytes > 128 || !line.ends_with('\n') {
        return Err("X11 input command exceeds 128 bytes or is incomplete".into());
      }
      if synchronized && line == "sync\n" {
        // Each SendEvent has already completed its checked server request.
        self.geometry()?;
        output.write_all(b"ok\n")?;
        output.flush()?;
      } else {
        self.dispatch(Command::parse(&line)?, synchronized)?;
      }
    }
  }
}

impl InputSource for WindowInput {
  fn run(self) -> Result<()> {
    self.run_channel(false)
  }

  fn run_synchronized(self) -> Result<()> {
    self.run_channel(true)
  }
}

impl WindowInput {
  fn run_channel(mut self, synchronized: bool) -> Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "ready")?;
    stdout.flush()?;
    let result = self.receive(&mut io::stdin().lock(), &mut stdout, synchronized);
    if self.buttons.is_empty() && self.keys.is_empty() {
      return result;
    }
    let release = self
      .geometry()
      .and_then(|geometry| self.release_all([geometry.width, geometry.height]));
    result.and(release)
  }
}

fn server_clock(conn: &RustConnection, root: Window) -> Result<(Timestamp, Instant)> {
  // Sample server time on an unmapped window owned by this connection.
  // Subsequent events can use a monotonic clock without another X round trip.
  let window = conn.generate_id()?;
  conn
    .create_window(
      0,
      window,
      root,
      0,
      0,
      1,
      1,
      0,
      WindowClass::INPUT_ONLY,
      0,
      &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?
    .check()?;
  conn
    .change_property(
      PropMode::REPLACE,
      window,
      AtomEnum::INTEGER,
      AtomEnum::INTEGER,
      32,
      1,
      &[0; 4],
    )?
    .check()?;
  loop {
    if let XEvent::PropertyNotify(event) = conn.wait_for_event()?
      && event.window == window
    {
      let instant = Instant::now();
      conn.destroy_window(window)?.check()?;
      return Ok((event.time, instant));
    }
  }
}

fn mask_matches(selected: EventMask, events: EventMask) -> bool {
  u32::from(selected & events) != 0
}

#[derive(Clone, Copy)]
enum InputEvent {
  Motion,
  Button(u8, bool),
  Key(u8, bool),
}

#[derive(Clone, Copy)]
struct FramePoint {
  point: [u32; 2],
  size: [u32; 2],
}

impl FramePoint {
  fn in_window(self, [window_width, window_height]: [u16; 2]) -> Result<[i16; 2]> {
    fn scale(point: u32, frame: u32, window: u16) -> Result<i16> {
      // The captured pixel's center keeps edges aligned when the target resizes.
      let coordinate = (2 * point + 1) * u32::from(window) / (2 * frame);
      Ok(i16::try_from(coordinate)?)
    }
    Ok([
      scale(self.point[0], self.size[0], window_width)?,
      scale(self.point[1], self.size[1], window_height)?,
    ])
  }
}

enum Command<'a> {
  Move(FramePoint),
  Button(u8, bool),
  Key(&'a str, bool),
  Reset,
}

impl<'a> Command<'a> {
  fn parse(line: &'a str) -> Result<Self> {
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    match fields.as_slice() {
      ["reset"] => Ok(Self::Reset),
      ["move", x, y, width, height] => {
        let [x, y, width, height] = [
          x.parse::<u32>()?,
          y.parse::<u32>()?,
          width.parse::<u32>()?,
          height.parse::<u32>()?,
        ];
        if !(1..=16384).contains(&width)
          || !(1..=16384).contains(&height)
          || width * height > 32 * 1024 * 1024
          || x >= width
          || y >= height
        {
          return Err("X11 pointer is outside valid capture dimensions".into());
        }
        Ok(Self::Move(FramePoint {
          point: [x, y],
          size: [width, height],
        }))
      }
      ["button", code, pressed @ ("0" | "1")] => {
        let code = code.parse::<u8>()?;
        if !(1..=9).contains(&code) {
          return Err("unknown X11 button".into());
        }
        Ok(Self::Button(code, *pressed == "1"))
      }
      ["key", name, pressed @ ("0" | "1")] => Ok(Self::Key(name, *pressed == "1")),
      _ => Err("unknown X11 input command".into()),
    }
  }
}

fn canonical_key(name: &str) -> &str {
  match name {
    "CapsLock" => "Caps_Lock",
    "NumLock" => "Num_Lock",
    "ScrollLock" => "Scroll_Lock",
    "PrintScreen" => "Print",
    "XF86Back" => "XF86XK_Back",
    "Page_Up" => "Prior",
    "Page_Down" => "Next",
    _ => name,
  }
}

struct Keyboard {
  codes: BTreeMap<&'static str, u8>,
  modifiers: [u16; 256],
  lock_mask: u16,
}

impl Keyboard {
  fn read(conn: &RustConnection) -> Result<Self> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let count = setup.max_keycode - min + 1;
    let mapping = conn.get_keyboard_mapping(min, count)?.reply()?;
    let stride = usize::from(mapping.keysyms_per_keycode);
    if stride == 0 {
      return Err("X11 keymap has no keysyms".into());
    }
    let mut codes = BTreeMap::new();
    // Visit primary symbols first so a secondary layout or keypad alias
    // cannot replace a key carrying the symbol in its base position.
    for column in 0..stride {
      for (index, &raw) in mapping
        .keysyms
        .iter()
        .skip(column)
        .step_by(stride)
        .enumerate()
      {
        if raw != 0
          && let Some(name) = xkeysym::Keysym::new(raw).name()
        {
          let name = name.strip_prefix("XK_").unwrap_or(name);
          codes.entry(name).or_insert(min + u8::try_from(index)?);
        }
      }
    }

    let reply = conn.get_modifier_mapping()?.reply()?;
    let mut modifiers = [0; 256];
    let count = usize::from(reply.keycodes_per_modifier());
    if count != 0 {
      for (bit, keys) in reply.keycodes.chunks_exact(count).enumerate() {
        for &code in keys.iter().filter(|&&code| code != 0) {
          modifiers[usize::from(code)] |= 1 << bit;
        }
      }
    }
    let lock_mask = ["Caps_Lock", "Num_Lock", "Scroll_Lock"]
      .into_iter()
      .filter_map(|name| codes.get(name))
      .fold(0, |state, &code| state | modifiers[usize::from(code)]);
    Ok(Self {
      codes,
      modifiers,
      lock_mask,
    })
  }

  fn modifier(&self, code: u8) -> u16 {
    self.modifiers[usize::from(code)]
  }
}
