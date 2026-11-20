#[cfg(target_os = "linux")]
mod linux;

#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
  linux::run()
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
  windows::run()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn main() -> std::process::ExitCode {
  eprintln!("ctream-agent's bundled drivers require a Linux or Windows display host");
  std::process::ExitCode::FAILURE
}
