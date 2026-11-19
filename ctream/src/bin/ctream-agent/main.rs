#[cfg(unix)]
mod unix;

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
  unix::run()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
  eprintln!("ctream-agent needs a Unix display host");
  std::process::ExitCode::FAILURE
}
