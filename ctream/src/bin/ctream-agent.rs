use ctream::protocol::{Session, Target, VERSION, X11_FRAME_SIZE};
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

fn capture(target: Target) -> io::Result<()> {
  let (notify, closed) = mpsc::channel();
  thread::spawn(move || {
    let _ = io::copy(&mut io::stdin().lock(), &mut io::sink());
    let _ = notify.send(());
  });
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let args: Vec<_> = env::args().skip(1).collect();
  match args.as_slice() {
    [version, operation] if version == VERSION && operation == "discover" => {
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
    [version, operation, target] if version == VERSION => {
      let target = Target::parse(target)?;
      match operation.as_str() {
        "capture" => capture(target)?,
        "input" => input(target)?,
        _ => return Err("unknown ctream-agent operation".into()),
      }
    }
    _ => {
      return Err("usage: ctream-agent 1.0.1 (discover | capture DISPLAY | input DISPLAY)".into());
    }
  }
  Ok(())
}
