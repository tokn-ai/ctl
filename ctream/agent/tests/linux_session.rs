#![cfg(target_os = "linux")]

use ctream_client::{
  Client,
  media::{Frame, VideoSource},
  transport::Local,
};
use ctream_proto::{Action, CaptureScope, Display, Target};
use std::{
  env, io,
  path::PathBuf,
  time::{Duration, Instant},
};

#[test]
#[ignore = "requires the disposable X11 and Wayland sessions in docker/ctream"]
fn native_sessions_stream_through_local_ipc() -> io::Result<()> {
  let client = Client::new(Local {
    agent: PathBuf::from(env!("CARGO_BIN_EXE_ctream-agent")),
  });
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
