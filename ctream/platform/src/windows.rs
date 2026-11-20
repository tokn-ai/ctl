pub mod x11;

use ctream_proto::{Target, Window, valid_window_id};
use std::{
  io,
  process::{Command, Stdio},
};

pub fn wayland_window(target: Target) -> io::Result<Command> {
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

pub fn list(target: Target) -> Result<Vec<Window>, Box<dyn std::error::Error>> {
  if matches!(target, Target::X11(_)) {
    return x11::list(&target.to_string());
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
