#[path = "ctream-agent/x11_window.rs"]
mod x11_window;

use ctream::protocol::{
  DISPLAY_VERSION, Session, Target, VERSION, WAYLAND_WINDOW_VERSION, Window, X11_FRAME_SIZE,
  valid_window_id,
};
use std::{
  env, fs, io,
  os::unix::{fs::MetadataExt, process::CommandExt},
  process::{Command, Stdio},
  sync::mpsc::{self, Receiver, RecvTimeoutError},
  thread,
  time::Duration,
};

fn x11_helper(display: u32) -> Command {
  let mut command = Command::new("python3");
  command
    .args(["-u", "-c", include_str!("../x11_input.py")])
    .env("DISPLAY", format!(":{display}"));
  command
}

fn input(target: Target) -> io::Result<()> {
  let error = match target {
    Target::X11(display) => x11_helper(display).exec(),
    Target::Wayland(number) => Command::new("ctream-wayland-input")
      .env_remove("WAYLAND_SOCKET")
      .env("WAYLAND_DISPLAY", format!("wayland-{number}"))
      .exec(),
  };
  Err(error)
}

// Capture owns the child until it exits or the client closes the channel.
fn run_capture(command: &mut Command, closed: &Receiver<()>) -> io::Result<bool> {
  let mut child = command.stdin(Stdio::null()).spawn()?;
  loop {
    if let Some(status) = child.try_wait()? {
      return if status.success() {
        Ok(true)
      } else {
        Err(io::Error::other(format!(
          "capture backend exited with {status}"
        )))
      };
    }
    if channel_closed(closed, Duration::from_millis(20)) {
      let _ = child.kill();
      child.wait()?;
      return Ok(false);
    }
  }
}

fn channel_closed(closed: &Receiver<()>, delay: Duration) -> bool {
  !matches!(closed.recv_timeout(delay), Err(RecvTimeoutError::Timeout))
}

fn capture_lifetime() -> Receiver<()> {
  let (notify, closed) = mpsc::channel();
  thread::spawn(move || {
    let _ = io::copy(&mut io::stdin().lock(), &mut io::sink());
    let _ = notify.send(());
  });
  closed
}

fn capture(target: Target) -> io::Result<()> {
  let closed = capture_lifetime();
  match target {
    Target::X11(display) => {
      let output = x11_helper(display)
        .arg("--geometry")
        .stdin(Stdio::null())
        .output()?;
      let geometry = String::from_utf8_lossy(&output.stdout);
      let dimensions: Vec<_> = geometry
        .split_ascii_whitespace()
        .map(str::parse::<u32>)
        .collect();
      let [Ok(width), Ok(height)] = dimensions.as_slice() else {
        return Err(io::Error::other(format!(
          "cannot read X11 geometry for :{display}: {}",
          String::from_utf8_lossy(&output.stderr).trim()
        )));
      };
      if !output.status.success() || *width == 0 || *height == 0 {
        return Err(io::Error::other("invalid X11 geometry"));
      }
      let [w, h] = X11_FRAME_SIZE;
      run_capture(
        Command::new("ffmpeg").args([
          "-nostdin",
          "-hide_banner",
          "-loglevel",
          "error",
          "-f",
          "x11grab",
          "-framerate",
          "8",
          "-video_size",
          &format!("{width}x{height}"),
          "-i",
          &format!(":{display}"),
          "-vf",
          &format!("scale={w}:{h}"),
          "-pix_fmt",
          "rgb24",
          "-f",
          "rawvideo",
          "pipe:1",
        ]),
        &closed,
      )?;
    }
    Target::Wayland(number) => {
      let mut grim = Command::new("grim");
      grim
        .env_remove("WAYLAND_SOCKET")
        .env("WAYLAND_DISPLAY", format!("wayland-{number}"))
        .args(["-t", "ppm", "-"]);
      while run_capture(&mut grim, &closed)? && !channel_closed(&closed, Duration::from_millis(125))
      {
      }
    }
  }
  Ok(())
}

fn wayland_window(target: Target) -> io::Result<Command> {
  let Target::Wayland(_) = target else {
    return Err(io::Error::other(
      "window operations currently require a supported Wayland compositor",
    ));
  };
  let mut command = Command::new("ctream-wayland-window");
  command
    .env_remove("WAYLAND_SOCKET")
    .env("WAYLAND_DISPLAY", target.to_string());
  Ok(command)
}

fn list_windows(target: Target) -> Result<Vec<Window>, Box<dyn std::error::Error>> {
  if matches!(target, Target::X11(_)) {
    return x11_window::list(&target.to_string());
  }
  let output = wayland_window(target)?
    .arg("list")
    .stdin(Stdio::null())
    .output()?;
  if !output.status.success() {
    return Err(
      format!(
        "window discovery failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      )
      .into(),
    );
  }
  let mut windows: Vec<Window> = serde_json::from_slice(&output.stdout)?;
  windows.retain(|window| valid_window_id(&window.id));
  Ok(windows)
}

fn window_operation(
  operation: &str,
  target: Target,
  id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
  if !valid_window_id(id) {
    return Err("invalid window identifier".into());
  }
  if operation == "capture-window" {
    if matches!(target, Target::X11(_)) {
      return x11_window::capture(&target.to_string(), id, &capture_lifetime());
    }
    run_capture(
      wayland_window(target)?.args(["capture", id]),
      &capture_lifetime(),
    )?;
    return Ok(());
  }
  if matches!(target, Target::X11(_)) {
    return Err("semantic actions currently require a supported Wayland compositor".into());
  }
  let window = list_windows(target)?
    .into_iter()
    .find(|window| window.id == id)
    .ok_or("selected window is no longer available")?;
  Err(
    Command::new("python3")
      .args([
        "-u",
        "-c",
        include_str!("../atspi.py"),
        operation,
        id,
        &window.title,
      ])
      .exec()
      .into(),
  )
}

fn supports_windows(version: &str, target: Target) -> bool {
  version == VERSION || (version == WAYLAND_WINDOW_VERSION && matches!(target, Target::Wayland(_)))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let args: Vec<_> = env::args().skip(1).collect();
  match args.as_slice() {
    [version, operation]
      if matches!(
        version.as_str(),
        VERSION | WAYLAND_WINDOW_VERSION | DISPLAY_VERSION
      ) && operation == "discover" =>
    {
      let runtime_uid = env::var_os("XDG_RUNTIME_DIR")
        .and_then(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.uid());
      let sessions: Vec<_> = ctream::discover()?
        .into_iter()
        .map(|(display, class)| {
          let selectable = match &display {
            ctream::Display::X11(_) => true,
            ctream::Display::Wayland { uid, .. } => runtime_uid == Some(*uid),
          };
          Session {
            display,
            class,
            selectable,
          }
        })
        .collect();
      serde_json::to_writer(io::stdout().lock(), &sessions)?;
    }
    [version, operation, target]
      if matches!(
        version.as_str(),
        VERSION | WAYLAND_WINDOW_VERSION | DISPLAY_VERSION
      ) =>
    {
      let target = Target::parse(target)?;
      match operation.as_str() {
        "capture" => capture(target)?,
        "input" => input(target)?,
        "windows" if supports_windows(version, target) => {
          serde_json::to_writer(io::stdout().lock(), &list_windows(target)?)?
        }
        _ => return Err("unknown ctream-agent operation".into()),
      }
    }
    [version, operation, target, id]
      if matches!(operation.as_str(), "capture-window" | "elements" | "action") =>
    {
      let target = Target::parse(target)?;
      if !supports_windows(version, target) {
        return Err("window operation unavailable in requested agent version".into());
      }
      window_operation(operation, target, id)?;
    }
    _ => return Err("usage: ctream-agent VERSION OPERATION [DISPLAY [WINDOW]]".into()),
  }
  Ok(())
}
