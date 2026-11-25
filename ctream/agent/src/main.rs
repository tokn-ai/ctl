#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod attributed;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn run() -> ctream_driver::Result<()> {
  use ctream_agent::activity;
  let args: Vec<String> = std::env::args().skip(1).collect();
  if args.first().is_some_and(|arg| arg == "activity") {
    let limit = match &args[1..] {
      [] => 50,
      [option, value] if option == "--limit" => value.parse()?,
      _ => return Err("usage: ctream-agent activity [--limit COUNT]".into()),
    };
    activity::write_recent(std::io::stdout().lock(), limit)?;
    return Ok(());
  }
  #[cfg(target_os = "macos")]
  {
    macos::run(&args)
  }
  #[cfg(target_os = "linux")]
  {
    linux::run(&args)
  }
  #[cfg(windows)]
  {
    windows::run(&args)
  }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn main() -> std::process::ExitCode {
  match run() {
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
