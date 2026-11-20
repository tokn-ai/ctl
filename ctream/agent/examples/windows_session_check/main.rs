#[cfg(windows)]
mod windows;

fn main() -> Result<(), Box<dyn std::error::Error>> {
  #[cfg(windows)]
  return windows::run();
  #[cfg(not(windows))]
  Err("windows-session-check requires Windows".into())
}
