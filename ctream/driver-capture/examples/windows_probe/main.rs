mod stdio;

#[cfg(windows)]
mod windows;

fn main() -> std::process::ExitCode {
  let mut args = std::env::args_os().skip(1);
  let result = match (args.next(), args.next()) {
    (None, None) => capture(),
    (Some(mode), None) if mode == "--echo-stdio" => stdio::echo().map_err(Into::into),
    _ => {
      eprintln!("usage: windows-probe [--echo-stdio]");
      return std::process::ExitCode::from(2);
    }
  };
  match result {
    Ok(()) => std::process::ExitCode::SUCCESS,
    Err(error) => {
      eprintln!("{error}");
      std::process::ExitCode::FAILURE
    }
  }
}

#[cfg(windows)]
fn capture() -> Result<(), Box<dyn std::error::Error>> {
  let report = windows::probe();
  serde_json::to_writer_pretty(std::io::stdout().lock(), &report)?;
  println!();
  Ok(())
}

#[cfg(not(windows))]
fn capture() -> Result<(), Box<dyn std::error::Error>> {
  Err("windows-probe requires Windows".into())
}
