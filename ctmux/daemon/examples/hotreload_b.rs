//! Replacement image for the isolated Unix PTY exec proof.

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "hotreload_support/mod.rs"]
mod hotreload_support;

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
  hotreload_support::run("B")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() {
  eprintln!("The PTY exec proof is supported on macOS and Linux.");
  std::process::exit(2);
}
