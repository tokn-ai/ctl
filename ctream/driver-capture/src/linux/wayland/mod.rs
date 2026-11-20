//! Independent Wayland display and toplevel-window capture providers.

pub mod grim;
pub mod image_copy;

use std::process::Command;

fn helper(display: u32, name: &str) -> Command {
  let mut command = Command::new(name);
  command
    .env_remove("WAYLAND_SOCKET")
    .env("WAYLAND_DISPLAY", format!("wayland-{display}"));
  command
}
