#[cfg(windows)]
mod windows;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
  let report = windows::probe();
  serde_json::to_writer_pretty(std::io::stdout().lock(), &report)?;
  println!();
  Ok(())
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
  eprintln!("windows-probe requires Windows");
  std::process::ExitCode::FAILURE
}
