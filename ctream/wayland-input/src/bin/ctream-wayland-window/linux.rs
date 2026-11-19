use serde::Serialize;
use std::{
  collections::HashMap,
  error::Error,
  io::{self, Write},
  os::{fd::AsFd, unix::fs::FileExt},
};
use wayland_client::{
  Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
  globals::{GlobalListContents, registry_queue_init},
  protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool},
};
use wayland_protocols::ext::{
  foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1 as handle, ext_foreign_toplevel_list_v1 as list,
  },
  image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1 as SourceManager,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1 as Source,
  },
  image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1 as frame, ext_image_copy_capture_manager_v1 as manager,
    ext_image_copy_capture_session_v1 as session,
  },
};

#[derive(Default, Serialize)]
struct Window {
  id: String,
  title: String,
  app_id: String,
  #[serde(skip)]
  ready: bool,
}

#[derive(Default)]
struct State {
  windows: HashMap<handle::ExtForeignToplevelHandleV1, Window>,
  finished: bool,
  size: (u32, u32),
  format: Option<wl_shm::Format>,
  constraints_ready: bool,
  stopped: bool,
  result: Option<Result<(), WEnum<frame::FailureReason>>>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
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

impl Dispatch<list::ExtForeignToplevelListV1, ()> for State {
  fn event(
    state: &mut Self,
    _: &list::ExtForeignToplevelListV1,
    event: list::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    if let list::Event::Finished = event {
      state.finished = true;
    }
  }
  wayland_client::event_created_child!(State, list::ExtForeignToplevelListV1, [
    0 => (handle::ExtForeignToplevelHandleV1, ())
  ]);
}

impl Dispatch<handle::ExtForeignToplevelHandleV1, ()> for State {
  fn event(
    state: &mut Self,
    proxy: &handle::ExtForeignToplevelHandleV1,
    event: handle::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    if let handle::Event::Closed = event {
      state.windows.remove(proxy);
      proxy.destroy();
      return;
    }
    let window = state.windows.entry(proxy.clone()).or_default();
    match event {
      handle::Event::Identifier { identifier } => window.id = identifier,
      handle::Event::Title { title } => window.title = title,
      handle::Event::AppId { app_id } => window.app_id = app_id,
      handle::Event::Done => window.ready = true,
      _ => {}
    }
  }
}

impl Dispatch<session::ExtImageCopyCaptureSessionV1, ()> for State {
  fn event(
    state: &mut Self,
    _: &session::ExtImageCopyCaptureSessionV1,
    event: session::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    match event {
      session::Event::BufferSize { width, height } => {
        state.size = (width, height);
        state.format = None;
        state.constraints_ready = false;
      }
      session::Event::ShmFormat {
        format: WEnum::Value(format @ (wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888)),
      } => state.format = Some(format),
      session::Event::Done => state.constraints_ready = true,
      session::Event::Stopped => state.stopped = true,
      _ => {}
    }
  }
}

impl Dispatch<frame::ExtImageCopyCaptureFrameV1, ()> for State {
  fn event(
    state: &mut Self,
    _: &frame::ExtImageCopyCaptureFrameV1,
    event: frame::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    match event {
      frame::Event::Ready => state.result = Some(Ok(())),
      frame::Event::Failed { reason } => state.result = Some(Err(reason)),
      _ => {}
    }
  }
}

delegate_noop!(State: ignore SourceManager);
delegate_noop!(State: ignore Source);
delegate_noop!(State: ignore manager::ExtImageCopyCaptureManagerV1);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);

pub(super) fn run(id: Option<&str>) -> Result<(), Box<dyn Error>> {
  let connection = Connection::connect_to_env()?;
  let (globals, mut queue) = registry_queue_init::<State>(&connection)?;
  let qh = queue.handle();
  let _list: list::ExtForeignToplevelListV1 = globals.bind(&qh, 1..=1, ())?;
  let sources: SourceManager = globals.bind(&qh, 1..=1, ())?;
  let manager: manager::ExtImageCopyCaptureManagerV1 = globals.bind(&qh, 1..=1, ())?;
  let mut state = State::default();
  queue.roundtrip(&mut state)?;
  if state.finished {
    return Err("compositor denied the window list".into());
  }
  let available = |window: &&Window| window.ready && !window.id.is_empty();
  let Some(id) = id else {
    let mut windows: Vec<_> = state.windows.values().filter(available).collect();
    windows.sort_by(|a, b| (&a.app_id, &a.title, &a.id).cmp(&(&b.app_id, &b.title, &b.id)));
    serde_json::to_writer(io::stdout().lock(), &windows)?;
    return Ok(());
  };
  let window = state
    .windows
    .iter()
    .find(|(_, w)| available(w) && w.id == id)
    .map(|(proxy, _)| proxy.clone())
    .ok_or("selected window is no longer available")?;
  let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ())?;
  let source = sources.create_source(&window, &qh, ());
  let session = manager.create_session(&source, manager::Options::empty(), &qh, ());
  let mut out = io::stdout().lock();
  let mut pixels = Vec::new();
  let mut rgb = Vec::new();
  while !state.stopped && state.windows.contains_key(&window) {
    if !state.constraints_ready {
      queue.blocking_dispatch(&mut state)?;
      continue;
    }
    let (width, height) = state.size;
    let count = width
      .checked_mul(height)
      .filter(|n| *n > 0 && *n <= 32 * 1024 * 1024 && width <= 16384 && height <= 16384)
      .ok_or("invalid capture buffer size")?;
    let format = state
      .format
      .ok_or("compositor offers no supported RGB shared-memory format")?;
    let file = tempfile::tempfile()?;
    file.set_len(u64::from(count) * 4)?;
    let pool = shm.create_pool(file.as_fd(), (count * 4) as i32, &qh, ());
    let buffer = pool.create_buffer(
      0,
      width as i32,
      height as i32,
      (width * 4) as i32,
      format,
      &qh,
      (),
    );
    pool.destroy();
    let frame = session.create_frame(&qh, ());
    frame.attach_buffer(&buffer);
    frame.damage_buffer(0, 0, width as i32, height as i32);
    frame.capture();
    while state.result.is_none() && !state.stopped && state.windows.contains_key(&window) {
      queue.blocking_dispatch(&mut state)?;
    }
    frame.destroy();
    buffer.destroy();
    if state.stopped || !state.windows.contains_key(&window) {
      break;
    }
    match state.result.take() {
      Some(Ok(())) => {}
      Some(Err(WEnum::Value(frame::FailureReason::BufferConstraints))) => continue,
      Some(Err(WEnum::Value(frame::FailureReason::Stopped))) => break,
      other => return Err(format!("toplevel capture failed: {other:?}").into()),
    }
    pixels.resize((count * 4) as usize, 0);
    file.read_exact_at(&mut pixels, 0)?;
    rgb.clear();
    for &pixel in pixels.as_chunks::<4>().0 {
      let [_, red, green, blue] = u32::from_ne_bytes(pixel).to_be_bytes();
      rgb.extend_from_slice(&[red, green, blue]);
    }
    write!(out, "P6\n{width} {height}\n255\n")?;
    out.write_all(&rgb)?;
    out.flush()?;
  }
  session.destroy();
  Ok(())
}
