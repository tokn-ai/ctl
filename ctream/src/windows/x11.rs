use crate::protocol::Window;
use std::{
  error::Error,
  io::{self, Write},
  time::{Duration, Instant},
};
use x11rb::{
  connection::Connection,
  errors::ReplyError,
  image::{Image, PixelLayout},
  protocol::{
    ErrorKind, Event,
    composite::{ConnectionExt as _, Redirect},
    xproto::{
      Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, MapState,
      VisualClass, WindowClass,
    },
  },
};

fn disappeared(error: &ReplyError) -> bool {
  matches!(error, ReplyError::X11Error(error) if error.error_kind == ErrorKind::Window)
}

fn property(
  conn: &impl Connection,
  window: u32,
  name: Atom,
  kind: impl Into<Atom>,
  limit: u32,
) -> Result<x11rb::protocol::xproto::GetPropertyReply, ReplyError> {
  conn
    .get_property(false, window, name, kind, 0, limit)?
    .reply()
}

fn top_levels(conn: &impl Connection, root: u32) -> Result<Vec<u32>, ReplyError> {
  let clients = conn.intern_atom(true, b"_NET_CLIENT_LIST")?.reply()?.atom;
  if clients != 0 {
    let reply = property(conn, root, clients, AtomEnum::WINDOW, 16384)?;
    if let Some(ids) = reply.value32() {
      return Ok(ids.collect());
    }
  }
  Ok(conn.query_tree(root)?.reply()?.children)
}

fn composite_version(conn: &impl Connection) -> Result<(), Box<dyn Error>> {
  let version = conn.composite_query_version(0, 4)?.reply()?;
  if (version.major_version, version.minor_version) < (0, 2) {
    return Err("X11 window capture requires Composite 0.2 or newer".into());
  }
  Ok(())
}

pub fn list(display: &str) -> Result<Vec<Window>, Box<dyn Error>> {
  let (conn, screen) = x11rb::connect(Some(display))?;
  composite_version(&conn)?;
  let root = conn.setup().roots[screen].root;
  let net_name = conn.intern_atom(true, b"_NET_WM_NAME")?.reply()?.atom;
  let utf8 = conn.intern_atom(true, b"UTF8_STRING")?.reply()?.atom;
  let read_metadata = |window| -> Result<Option<Window>, ReplyError> {
    let attrs = conn.get_window_attributes(window)?.reply()?;
    if attrs.class != WindowClass::INPUT_OUTPUT
      || attrs.override_redirect
      || attrs.map_state != MapState::VIEWABLE
    {
      return Ok(None);
    }
    let mut title = Vec::new();
    if net_name != 0 && utf8 != 0 {
      title = property(&conn, window, net_name, utf8, 1024)?.value;
    }
    let title = if title.is_empty() {
      let legacy = property(
        &conn,
        window,
        AtomEnum::WM_NAME.into(),
        AtomEnum::STRING,
        1024,
      )?
      .value;
      legacy.into_iter().map(char::from).collect()
    } else {
      String::from_utf8_lossy(&title).into_owned()
    };
    let class = property(
      &conn,
      window,
      AtomEnum::WM_CLASS.into(),
      AtomEnum::STRING,
      1024,
    )?
    .value;
    let app_id = class
      .split(|byte| *byte == 0)
      .rfind(|part| !part.is_empty())
      .map(|part| part.iter().copied().map(char::from).collect())
      .unwrap_or_default();
    Ok(Some(Window {
      id: format!("x11-{window:x}"),
      title,
      app_id,
    }))
  };
  let mut windows = Vec::new();
  for id in top_levels(&conn, root)? {
    match read_metadata(id) {
      Ok(Some(window)) => windows.push(window),
      Ok(None) => {}
      Err(error) if disappeared(&error) => {}
      Err(error) => return Err(error.into()),
    }
  }
  Ok(windows)
}

pub fn capture(
  display: &str,
  id: &str,
  mut closed: impl FnMut(Duration) -> bool,
) -> Result<(), Box<dyn Error>> {
  let id = id
    .strip_prefix("x11-")
    .ok_or("invalid X11 window identifier")?;
  let window = u32::from_str_radix(id, 16)?;
  let (conn, screen) = x11rb::connect(Some(display))?;
  composite_version(&conn)?;
  let screen = &conn.setup().roots[screen];
  if !top_levels(&conn, screen.root)?.contains(&window) {
    return Err("selected X11 window is no longer available".into());
  }
  let attrs = conn.get_window_attributes(window)?.reply()?;
  if attrs.class != WindowClass::INPUT_OUTPUT || attrs.override_redirect {
    return Err("selected X11 window is not capturable".into());
  }
  let visual = screen
    .allowed_depths
    .iter()
    .flat_map(|depth| &depth.visuals)
    .find(|visual| visual.visual_id == attrs.visual && visual.class == VisualClass::TRUE_COLOR)
    .ok_or("X11 window capture requires a TrueColor visual")?;
  let layout = PixelLayout::from_visual_type(*visual)?;

  conn
    .change_window_attributes(
      window,
      &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
    )?
    .check()?;
  conn
    .composite_redirect_window(window, Redirect::AUTOMATIC)?
    .check()?;

  let mut out = io::stdout().lock();
  let mut rgb = Vec::new();
  let interval = Duration::from_millis(125);
  let mut viewable = attrs.map_state == MapState::VIEWABLE;
  loop {
    let started = Instant::now();
    while let Some(event) = conn.poll_for_event()? {
      match event {
        Event::DestroyNotify(event) if event.window == window => return Ok(()),
        Event::UnmapNotify(event) if event.window == window => viewable = false,
        Event::MapNotify(event) if event.window == window => viewable = true,
        _ => {}
      }
    }
    if viewable {
      // Composite replaces the backing pixmap on resize. Naming it anew makes
      // the pixmap and its geometry a consistent snapshot of this frame.
      let pixmap = conn.generate_id()?;
      match conn.composite_name_window_pixmap(window, pixmap)?.check() {
        Ok(()) => {
          let geometry = conn.get_geometry(pixmap)?.reply()?;
          let (width, height) = (geometry.width, geometry.height);
          if width > 16384
            || height > 16384
            || u32::from(width) * u32::from(height) > 32 * 1024 * 1024
          {
            return Err("X11 window exceeds the capture size limit".into());
          }
          let (image, _) = Image::get(&conn, pixmap, 0, 0, width, height)?;
          conn.free_pixmap(pixmap)?.check()?;
          rgb.clear();
          rgb.reserve(usize::from(width) * usize::from(height) * 3);
          for y in 0..height {
            for x in 0..width {
              let (r, g, b) = layout.decode(image.get_pixel(x, y));
              rgb.extend_from_slice(&[(r >> 8) as u8, (g >> 8) as u8, (b >> 8) as u8]);
            }
          }
          write!(out, "P6\n{width} {height}\n255\n")?;
          out.write_all(&rgb)?;
          out.flush()?;
        }
        Err(error) if disappeared(&error) => return Ok(()),
        // An unmap can arrive between polling and naming the pixmap.
        Err(ReplyError::X11Error(error)) if error.error_kind == ErrorKind::Match => {}
        Err(error) => return Err(error.into()),
      }
    }
    if closed(interval.saturating_sub(started.elapsed())) {
      return Ok(());
    }
  }
}
