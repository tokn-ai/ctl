#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
  linux::run()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
  eprintln!("ctream-wayland-input requires Linux");
  std::process::ExitCode::FAILURE
}
