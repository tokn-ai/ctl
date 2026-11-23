#![cfg(target_os = "linux")]

use ctream_client::{
  Client,
  media::{Frame, VideoSource},
  transport::Local,
};
use ctream_proto::{Action, CaptureScope, CaptureSystem, Display, Target};
use std::{
  env,
  error::Error,
  io,
  path::PathBuf,
  thread,
  time::{Duration, Instant},
};

#[test]
#[ignore = "requires the disposable X11 and Wayland sessions in docker/ctream"]
fn native_sessions_stream_through_local_ipc() -> io::Result<()> {
  let client = Client::new(Local {
    agent: PathBuf::from(env!("CARGO_BIN_EXE_ctream-agent")),
  });
  let catalog = client.capture_catalog()?;
  assert_eq!(catalog.len(), 4, "all compiled Linux capture providers");
  assert!(catalog.iter().all(|driver| driver.systems.len() == 1));
  for system in [CaptureSystem::X11, CaptureSystem::Wayland] {
    let drivers: Vec<_> = catalog
      .iter()
      .filter(|driver| driver.systems.contains(&system))
      .collect();
    assert_eq!(drivers.len(), 2);
    assert!(
      drivers
        .iter()
        .any(|driver| driver.info.scopes == [CaptureScope::Display])
    );
    assert!(
      drivers
        .iter()
        .any(|driver| driver.info.scopes == [CaptureScope::Window])
    );
  }
  let x11 = env::var("CTREAM_TEST_X11").unwrap_or_else(|_| ":98".into());
  let wayland = env::var("WAYLAND_DISPLAY").expect("fixture Wayland display");
  let displays = client.discover()?;
  assert_eq!(
    displays
      .iter()
      .filter(|session| match &session.display {
        Display::X11(number) => format!(":{number}") == x11,
        _ => false,
      })
      .count(),
    1,
    "filesystem and abstract X11 sockets must identify one display",
  );
  assert!(displays.iter().any(|session| match &session.display {
    Display::Wayland { socket, .. } => socket == &wayland,
    _ => false,
  }));
  for display in [x11, wayland] {
    let target = Target::parse(&display).unwrap();
    let session = client.session(target, None);
    let mut input = session.input()?;
    input.reset()?;
    drop(input);
    let drivers = client.capture_drivers(target)?;
    assert_eq!(
      drivers,
      catalog
        .iter()
        .filter(|driver| driver.systems.contains(&target.system()))
        .map(|driver| driver.info.clone())
        .collect::<Vec<_>>(),
      "retained target catalog must match the platform-wide descriptors",
    );
    let display_driver = drivers
      .iter()
      .find(|driver| driver.scopes.contains(&CaptureScope::Display))
      .expect("display provider");
    let window_driver = drivers
      .iter()
      .find(|driver| driver.scopes.contains(&CaptureScope::Window))
      .expect("window provider");
    assert!(session.capture_with_driver(&window_driver.id).is_err());
    assert!(session.capture_with_driver("user.missing").is_err());
    let mut selected = session.capture_with_driver("auto")?;
    assert_eq!(selected.driver(), Some(display_driver.id.as_str()));
    let mut selected_frame = Frame::default();
    selected.read_frame(&mut selected_frame)?;
    assert_eq!(
      selected_frame.pixels.len(),
      selected_frame.size[0] * selected_frame.size[1] * 3
    );
    drop(selected);
    let (mut capture, lifetime) = session.capture()?.split();
    let mut frame = Frame::default();
    for _ in 0..2 {
      capture.read_frame(&mut frame)?;
      assert_eq!(frame.pixels.len(), frame.size[0] * frame.size[1] * 3);
    }
    drop(lifetime);
    let closed = Instant::now();
    while capture.read_frame(&mut frame).is_ok() {
      assert!(
        closed.elapsed() < Duration::from_secs(5),
        "display capture did not stop"
      );
    }
    assert!(closed.elapsed() < Duration::from_secs(5));

    let window = client
      .windows(target)?
      .into_iter()
      .next()
      .expect("fixture window");
    let session = client.session(target, Some(window));
    assert!(session.capture_with_driver(&display_driver.id).is_err());
    let selected = session.capture_with_driver(&window_driver.id)?;
    assert_eq!(selected.driver(), Some(window_driver.id.as_str()));
    let (mut selected, selected_lifetime) = selected.split();
    selected.read_frame(&mut frame)?;
    assert_eq!(frame.pixels.len(), frame.size[0] * frame.size[1] * 3);
    drop(selected_lifetime);
    let closed = Instant::now();
    while selected.read_frame(&mut frame).is_ok() {
      assert!(
        closed.elapsed() < Duration::from_secs(5),
        "selected capture did not stop"
      );
    }
    assert!(closed.elapsed() < Duration::from_secs(5));
    let (mut capture, lifetime) = session.capture()?.split();
    capture.read_frame(&mut frame)?;
    assert_eq!(frame.pixels.len(), frame.size[0] * frame.size[1] * 3);
    assert!(frame.size.iter().all(|size| *size > 0));
    drop(lifetime);
    let closed = Instant::now();
    while capture.read_frame(&mut frame).is_ok() {
      assert!(
        closed.elapsed() < Duration::from_secs(5),
        "window capture did not stop"
      );
    }
    assert!(closed.elapsed() < Duration::from_secs(5));
    if session.supports_semantics() {
      let elements = session.elements()?;
      let entry = elements
        .iter()
        .find(|element| element.editable)
        .expect("editable fixture entry");
      let button = elements
        .iter()
        .find(|element| element.name == "Fixture button")
        .expect("fixture button");
      for element in [entry, button] {
        let bounds = element.bounds.expect("normalized control bounds");
        assert!(
          [bounds.x, bounds.y, bounds.width, bounds.height]
            .iter()
            .all(|value| value.is_finite())
        );
        assert!(bounds.width > 0.0 && bounds.height > 0.0);
      }
      assert!(
        session
          .action(Action::SetText {
            path: entry.path.clone(),
            text: "Local IPC test".into(),
          })?
          .applied
      );
      assert!(
        session
          .action(Action::Invoke {
            path: button.path.clone(),
            index: 0,
            name: button.actions.first().expect("button action").clone(),
          })?
          .applied
      );
      assert!(
        session
          .elements()?
          .iter()
          .any(|element| element.name == "Clicked")
      );
    }
  }
  Ok(())
}

#[test]
#[ignore = "requires the disposable X11 session and FFmpeg in docker/ctream"]
fn compressed_x11_display_and_window_honor_requested_frame_rate() -> Result<(), Box<dyn Error>> {
  use ctream_proto::{VideoCodec, VideoOptions, WindowId};
  use std::sync::mpsc;
  use x11rb::{
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT,
    connection::Connection as _,
    protocol::xproto::{ConnectionExt as _, CreateGCAux, CreateWindowAux, Rectangle, WindowClass},
  };

  const BAND_WIDTH: u16 = 32;
  const BANDS: usize = 10;
  const SIZE: [u16; 2] = [BANDS as u16 * BAND_WIDTH, 128];
  const WARMUP: Duration = Duration::from_millis(750);
  const OBSERVATION: Duration = Duration::from_secs(5);
  const PAINT_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 120);

  let client = Client::new(Local {
    agent: PathBuf::from(env!("CARGO_BIN_EXE_ctream-agent")),
  });
  let display = env::var("CTREAM_TEST_X11").unwrap_or_else(|_| ":98".into());
  let target = Target::parse(&display)?;
  let (conn, screen) = x11rb::connect(Some(&display))?;
  let screen = &conn.setup().roots[screen];
  let window = conn.generate_id()?;
  let pixmap = conn.generate_id()?;
  let black = conn.generate_id()?;
  let white = conn.generate_id()?;
  let [width, height] = SIZE;
  conn
    .create_window(
      COPY_DEPTH_FROM_PARENT,
      window,
      screen.root,
      200,
      90,
      width,
      height,
      0,
      WindowClass::INPUT_OUTPUT,
      COPY_FROM_PARENT,
      &CreateWindowAux::new().background_pixel(screen.black_pixel),
    )?
    .check()?;
  conn
    .create_pixmap(screen.root_depth, pixmap, window, width, height)?
    .check()?;
  conn
    .create_gc(
      black,
      window,
      &CreateGCAux::new()
        .foreground(screen.black_pixel)
        .graphics_exposures(0),
    )?
    .check()?;
  conn
    .create_gc(
      white,
      window,
      &CreateGCAux::new().foreground(screen.white_pixel),
    )?
    .check()?;
  conn.map_window(window)?.check()?;
  let fixture = wait_for("animated X11 fixture discovery", || {
    Ok(
      client
        .windows(target)?
        .into_iter()
        .find(|item| WindowId::x11_xid(&item.id) == Some(window)),
    )
  })?;
  let location = conn
    .translate_coordinates(window, screen.root, 0, 0)?
    .reply()?;
  let display_origin = [
    usize::try_from(location.dst_x)?,
    usize::try_from(location.dst_y)?,
  ];
  let display_size = [screen.width_in_pixels, screen.height_in_pixels].map(usize::from);
  let bands: [Rectangle; BANDS] = std::array::from_fn(|index| Rectangle {
    x: index as i16 * BAND_WIDTH as i16,
    y: 0,
    width: BAND_WIDTH,
    height,
  });
  let canvas = Rectangle {
    x: 0,
    y: 0,
    width,
    height,
  };

  let measured = thread::scope(|scope| -> Result<_, Box<dyn Error>> {
    let (stop_animation, stop) = mpsc::channel::<()>();
    let animator = scope.spawn(move || -> Result<_, Box<dyn Error + Send + Sync>> {
      let started = Instant::now();
      let mut painted = 0;
      let mut counter = 0u8;
      let mut lit = Vec::with_capacity(BANDS - 1);
      loop {
        let paint_started = Instant::now();
        lit.clear();
        // First and last bands identify the marker. The middle bands encode
        // a fresh value for every paint, before the complete image is copied.
        lit.push(bands[0]);
        for (bit, &band) in bands[1..BANDS - 1].iter().enumerate() {
          if counter & (1 << bit) != 0 {
            lit.push(band);
          }
        }
        conn.poly_fill_rectangle(pixmap, black, &[canvas])?;
        conn.poly_fill_rectangle(pixmap, white, &lit)?;
        conn.copy_area(pixmap, window, black, 0, 0, 0, 0, width, height)?;
        conn.flush()?;
        painted += 1;
        counter = counter.wrapping_add(1);
        let delay = PAINT_INTERVAL.saturating_sub(paint_started.elapsed());
        if stop.recv_timeout(delay) != Err(mpsc::RecvTimeoutError::Timeout) {
          return Ok(f64::from(painted) / started.elapsed().as_secs_f64());
        }
      }
    });
    let mut measured = Vec::new();
    for (driver, window, origin, size) in [
      ("linux.x11.ffmpeg", None, display_origin, display_size),
      (
        "linux.x11.composite",
        Some(fixture),
        [0, 0],
        SIZE.map(usize::from),
      ),
    ] {
      let session = client.session(target, window);
      let marker = |frame: &Frame| {
        let y = (origin[1] + usize::from(height / 2)) * frame.size[1] / size[1];
        let mut code = 0u16;
        for band in 0..BANDS {
          let x = origin[0] + band * usize::from(BAND_WIDTH) + usize::from(BAND_WIDTH / 2);
          let x = x * frame.size[0] / size[0];
          code = code << 1 | u16::from(frame.pixels[(y * frame.size[0] + x) * 3] > 128);
        }
        (code & 0b10_0000_0001 == 0b10_0000_0000).then_some(code)
      };
      let mut rates = [[0.0; 2]; 2];
      for (rate, max_fps) in rates.iter_mut().zip([30, 8]) {
        let mut capture = session.capture_with_options(
          driver,
          VideoOptions {
            max_fps,
            ..VideoOptions::default()
          },
        )?;
        let video = capture.video().expect("compressed X11 stream");
        assert_eq!(video.codec, VideoCodec::Av1, "{}", video.encoder);
        let mut frame = Frame::default();
        let warming = Instant::now();
        while warming.elapsed() < WARMUP {
          capture.read_frame(&mut frame)?;
        }
        let mut previous = marker(&frame);
        let started = Instant::now();
        let mut fresh = 0;
        let mut delivered = 0;
        loop {
          capture.read_frame(&mut frame)?;
          if started.elapsed() >= OBSERVATION {
            break;
          }
          delivered += 1;
          // The counter wraps, so compare adjacent valid markers.
          if let Some(code) = marker(&frame)
            && previous.replace(code).is_some_and(|last| last != code)
          {
            fresh += 1;
          }
        }
        let seconds = OBSERVATION.as_secs_f64();
        let fresh_rate = f64::from(fresh) / seconds;
        let delivered_rate = f64::from(delivered) / seconds;
        *rate = [fresh_rate, delivered_rate];
        eprintln!(
          "{driver} at {max_fps} FPS: {fresh}/{delivered} fresh frames, {fresh_rate:.1} fresh and {delivered_rate:.1} delivered FPS"
        );
      }
      measured.push((driver, rates));
    }
    drop(stop_animation);
    let paint_rate = animator
      .join()
      .expect("animated X11 fixture thread")
      .map_err(|error| -> Box<dyn Error> { error })?;
    eprintln!("X11 fixture submitted {paint_rate:.1} paints/s");
    Ok(measured)
  })?;
  for (driver, [fast, slow]) in measured {
    for (max_fps, [fresh, delivered], expected) in [(30, fast, 27.0..=33.0), (8, slow, 5.0..=9.0)] {
      assert!(
        expected.contains(&delivered),
        "{driver} at {max_fps} FPS delivered {delivered:.1}"
      );
      // Lossy AV1 can distort marker IDs. Check enough real changes to catch a
      // source that still samples at 8 FPS and repeats frames at 30 FPS.
      assert!(
        fresh >= delivered * 0.8,
        "{driver} at {max_fps} FPS delivered {fresh:.1} fresh of {delivered:.1} total FPS"
      );
    }
  }
  Ok(())
}

#[test]
#[ignore = "requires the disposable X11 session and FFmpeg in docker/ctream"]
fn compressed_x11_window_streams_live_and_preserves_resized_input() -> Result<(), Box<dyn Error>> {
  use ctream_proto::{VideoCodec, VideoOptions, WindowId};
  use x11rb::{
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT,
    connection::Connection as _,
    protocol::{
      Event,
      xproto::{ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, WindowClass},
    },
  };

  let client = Client::new(Local {
    agent: PathBuf::from(env!("CARGO_BIN_EXE_ctream-agent")),
  });
  let display = env::var("CTREAM_TEST_X11").unwrap_or_else(|_| ":98".into());
  let target = Target::parse(&display)?;
  let (conn, screen) = x11rb::connect(Some(&display))?;
  let screen = &conn.setup().roots[screen];
  let tint: [u8; 3] = [32, 167, 214];
  let [red, green, blue] = tint.map(|channel| u16::from(channel) * 257);
  let pixel = conn
    .alloc_color(screen.default_colormap, red, green, blue)?
    .reply()?
    .pixel;
  let fixture = conn.generate_id()?;
  let initial = [185, 111];
  let resized = [277u16, 159];
  conn
    .create_window(
      COPY_DEPTH_FROM_PARENT,
      fixture,
      screen.root,
      200,
      90,
      initial[0],
      initial[1],
      0,
      WindowClass::INPUT_OUTPUT,
      COPY_FROM_PARENT,
      &CreateWindowAux::new()
        .background_pixel(pixel)
        .event_mask(EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE),
    )?
    .check()?;
  conn.map_window(fixture)?.check()?;
  let window = wait_for("compressed X11 fixture discovery", || {
    Ok(
      client
        .windows(target)?
        .into_iter()
        .find(|window| WindowId::x11_xid(&window.id) == Some(fixture)),
    )
  })?;
  let session = client.session(target, Some(window));
  let capture = session.capture_with_options("linux.x11.composite", VideoOptions::default())?;
  assert_eq!(capture.driver(), Some("linux.x11.composite"));
  let video = capture.video().expect("compressed window stream");
  assert_eq!(video.codec, VideoCodec::Av1, "{}", video.encoder);
  let (mut capture, _lifetime) = capture.split();
  let mut frame = Frame::default();
  capture.read_frame(&mut frame)?;
  assert_eq!(frame.size, initial.map(usize::from));

  conn
    .configure_window(
      fixture,
      &ConfigureWindowAux::new()
        .width(u32::from(resized[0]))
        .height(u32::from(resized[1])),
    )?
    .check()?;
  let resized = resized.map(usize::from);
  wait_for("resized X11 video frame", || {
    capture.read_frame(&mut frame)?;
    Ok((frame.size == resized).then_some(()))
  })?;
  assert_eq!(frame.pixels.len(), resized[0] * resized[1] * 3);
  let sample = (64 * resized[0] + 96) * 3;
  for (actual, expected) in frame.pixels[sample..sample + 3].iter().zip(tint) {
    assert!(
      actual.abs_diff(expected) < 25,
      "resized frame fixture color"
    );
  }

  let mut input = session.input()?;
  input.move_pointer(96, 64, frame.size.map(|dimension| dimension as u32))?;
  input.button(1, true)?;
  input.button(1, false)?;
  let click = wait_for("click at resized video position", || {
    while let Some(event) = conn.poll_for_event()? {
      if let Event::ButtonPress(click) = event {
        return Ok(Some(click));
      }
    }
    Ok(None)
  })?;
  assert_eq!(
    (click.event, click.detail, click.event_x, click.event_y),
    (fixture, 1, 96, 64)
  );
  drop(input);
  conn.destroy_window(fixture)?.check()?;
  let closed = Instant::now();
  while capture.try_read_frame(&mut frame)? {
    assert!(closed.elapsed() < Duration::from_secs(5));
  }
  assert!(closed.elapsed() < Duration::from_secs(5));
  Ok(())
}

#[test]
#[ignore = "requires the disposable X11 session in docker/ctream"]
fn covered_x11_window_receives_input_without_changing_desktop_state() -> Result<(), Box<dyn Error>>
{
  use ctream_proto::WindowId;
  use x11rb::{
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT,
    connection::Connection as _,
    properties::{WmHints, WmSizeHints, WmSizeHintsSpecification},
    protocol::{
      Event,
      xproto::{
        AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Visibility, WindowClass,
      },
    },
    wrapper::ConnectionExt as _,
  };

  let client = Client::new(Local {
    agent: PathBuf::from(env!("CARGO_BIN_EXE_ctream-agent")),
  });
  let display = env::var("CTREAM_TEST_X11").unwrap_or_else(|_| ":98".into());
  let target = Target::parse(&display).expect("fixture X11 display");
  let (conn, screen) = x11rb::connect(Some(&display))?;
  let (cover_conn, _) = x11rb::connect(Some(&display))?;
  let screen = &conn.setup().roots[screen];
  let root = screen.root;
  let snapshot = || -> Result<_, Box<dyn Error>> {
    let pointer = conn.query_pointer(root)?.reply()?;
    let focus = conn.get_input_focus()?.reply()?;
    let keys = conn.query_keymap()?.reply()?.keys;
    Ok((
      pointer.root_x,
      pointer.root_y,
      pointer.mask,
      focus.focus,
      focus.revert_to,
      keys,
    ))
  };
  let before = snapshot()?;
  let x = if before.0 < screen.width_in_pixels as i16 / 2 {
    screen.width_in_pixels as i16 - 216
  } else {
    36
  };
  let fixture = conn.generate_id()?;
  let input_mask = EventMask::BUTTON_PRESS
    | EventMask::BUTTON_RELEASE
    | EventMask::KEY_PRESS
    | EventMask::KEY_RELEASE;
  conn
    .create_window(
      COPY_DEPTH_FROM_PARENT,
      fixture,
      root,
      x,
      45,
      180,
      120,
      0,
      WindowClass::INPUT_OUTPUT,
      COPY_FROM_PARENT,
      &CreateWindowAux::new()
        .background_pixel(screen.white_pixel)
        .event_mask(input_mask | EventMask::VISIBILITY_CHANGE),
    )?
    .check()?;
  WmHints {
    input: Some(false),
    ..WmHints::default()
  }
  .set(&conn, fixture)?
  .check()?;
  WmSizeHints {
    position: Some((WmSizeHintsSpecification::UserSpecified, x.into(), 45)),
    ..WmSizeHints::default()
  }
  .set_normal_hints(&conn, fixture)?
  .check()?;
  let user_time = conn.intern_atom(false, b"_NET_WM_USER_TIME")?.reply()?.atom;
  conn
    .change_property32(
      PropMode::REPLACE,
      fixture,
      user_time,
      AtomEnum::CARDINAL,
      &[0],
    )?
    .check()?;
  conn
    .change_property8(
      PropMode::REPLACE,
      fixture,
      AtomEnum::WM_NAME,
      AtomEnum::STRING,
      b"Ctream X11 input fixture",
    )?
    .check()?;
  conn.map_window(fixture)?.check()?;

  let window = wait_for("X11 fixture discovery", || {
    Ok(
      client
        .windows(target)?
        .into_iter()
        .find(|window| WindowId::x11_xid(&window.id) == Some(fixture)),
    )
  })?;
  wait_for("unobscured fixture", || {
    while let Some(event) = conn.poll_for_event()? {
      if matches!(event, Event::VisibilityNotify(event) if event.state == Visibility::UNOBSCURED) {
        return Ok(Some(()));
      }
    }
    Ok(None)
  })?;
  let geometry = conn.get_geometry(fixture)?.reply()?;
  let origin = conn.translate_coordinates(fixture, root, 0, 0)?.reply()?;
  let cover = cover_conn.generate_id()?;
  cover_conn
    .create_window(
      COPY_DEPTH_FROM_PARENT,
      cover,
      root,
      origin.dst_x,
      origin.dst_y,
      geometry.width,
      geometry.height,
      0,
      WindowClass::INPUT_OUTPUT,
      COPY_FROM_PARENT,
      &CreateWindowAux::new()
        .background_pixel(screen.black_pixel)
        .event_mask(input_mask)
        .override_redirect(1),
    )?
    .check()?;
  cover_conn.map_window(cover)?.check()?;
  wait_for("fully covered fixture", || {
    while let Some(event) = conn.poll_for_event()? {
      if matches!(event, Event::VisibilityNotify(event) if event.state == Visibility::FULLY_OBSCURED)
      {
        return Ok(Some(()));
      }
    }
    Ok(None)
  })?;
  assert_ne!(before.3, fixture);
  assert_eq!(snapshot()?, before);

  let session = client.session(target, Some(window));
  let mut input = session.input()?;
  assert_eq!(input.dimensions(), None);
  input.move_pointer(52, 37, [geometry.width.into(), geometry.height.into()])?;
  input.button(1, true)?;
  input.key("h", true)?;
  let next_input = || {
    wait_for("directed X11 input event", || {
      while let Some(event) = conn.poll_for_event()? {
        if matches!(
          event,
          Event::ButtonPress(_)
            | Event::ButtonRelease(_)
            | Event::KeyPress(_)
            | Event::KeyRelease(_)
        ) {
          return Ok(Some(event));
        }
      }
      Ok(None)
    })
  };
  let Event::ButtonPress(click) = next_input()? else {
    panic!("expected a button press");
  };
  assert_eq!(click.response_type & 0x80, 0x80, "SendEvent marker");
  assert_eq!(
    (click.event, click.detail, click.event_x, click.event_y),
    (fixture, 1, 52, 37)
  );
  let Event::KeyPress(h) = next_input()? else {
    panic!("expected a key press");
  };
  assert_eq!(
    snapshot()?,
    before,
    "pointer, focus and physical input state while pressed"
  );

  input.key("h", false)?;
  input.button(1, false)?;
  input.key("i", true)?;
  input.key("i", false)?;
  let Event::KeyRelease(release_h) = next_input()? else {
    panic!("expected first key release");
  };
  let Event::ButtonRelease(release_click) = next_input()? else {
    panic!("expected button release");
  };
  let Event::KeyPress(i) = next_input()? else {
    panic!("expected second key press");
  };
  let Event::KeyRelease(release_i) = next_input()? else {
    panic!("expected second key release");
  };
  assert_eq!(release_click.response_type & 0x80, 0x80);
  assert_eq!(
    (
      release_click.event,
      release_click.detail,
      release_click.event_x,
      release_click.event_y
    ),
    (fixture, 1, 52, 37)
  );
  for (press, release, expected) in [(h, release_h, 'h'), (i, release_i, 'i')] {
    for event in [press, release] {
      assert_eq!(event.event, fixture);
      assert_eq!(event.response_type & 0x80, 0x80);
    }
    assert_eq!(release.detail, press.detail);
    let mapping = conn.get_keyboard_mapping(press.detail, 1)?.reply()?;
    assert_eq!(mapping.keysyms.first().copied(), Some(u32::from(expected)));
  }
  assert!(
    cover_conn.poll_for_event()?.is_none(),
    "cover received directed input"
  );
  drop(input);
  drop(cover_conn);
  conn.destroy_window(fixture)?.check()?;
  assert_eq!(
    snapshot()?,
    before,
    "pointer, focus and physical input state after cleanup"
  );
  Ok(())
}

fn wait_for<T>(
  description: &str,
  mut check: impl FnMut() -> Result<Option<T>, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    if let Some(value) = check()? {
      return Ok(value);
    }
    if Instant::now() >= deadline {
      return Err(io::Error::new(io::ErrorKind::TimedOut, description).into());
    }
    thread::sleep(Duration::from_millis(20));
  }
}
