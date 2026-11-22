#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

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

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
  match macos::run() {
    Ok(()) => std::process::ExitCode::SUCCESS,
    Err(error) => {
      eprintln!("ctream-agent: {error}");
      std::process::ExitCode::FAILURE
    }
  }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn main() -> std::process::ExitCode {
  eprintln!("ctream-agent requires a Linux, macOS, or Windows host");
  std::process::ExitCode::FAILURE
}
