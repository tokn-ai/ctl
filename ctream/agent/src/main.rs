#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod attributed;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod viewer;
#[cfg(windows)]
mod windows;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn run() -> ctream_driver::Result<()> {
  use ctream_agent::activity;
  let args: Vec<String> = std::env::args().skip(1).collect();
  #[cfg(target_os = "linux")]
  if args == ["monitor-native-helper"] {
    return Ok(ctream_agent::native_input::monitor_linux_helper()?);
  }
  if args == ["daemon"] {
    return Ok(ctream_agent::daemon::run_daemon()?);
  }
  if args.first().is_some_and(|arg| arg == "daemon") {
    use ctream_agent::daemon;
    match &args[1..] {
      [op] if op == "status" => daemon::status()?,
      [op] if op == "upgrade" => daemon::upgrade(None)?,
      [op, path] if op == "upgrade" => daemon::upgrade(Some(path))?,
      [op] if op == "standby" => {
        if let Err(error) = daemon::standby() {
          println!("{}", serde_json::to_string(&error.to_string())?);
          return Err(error.into());
        }
      }
      _ => return Err("usage: ctream-agent daemon [status | upgrade [EXECUTABLE]]".into()),
    }
    return Ok(());
  }
  #[cfg(windows)]
  if args == ["ensure-daemon"] {
    return Ok(ctream_agent::daemon::ensure_daemon()?);
  }
  if args.first().is_some_and(|arg| arg == "activity") {
    let (details, options) = match &args[1..] {
      [flag, rest @ ..] if flag == "--details" => (true, rest),
      rest => (false, rest),
    };
    let limit = match options {
      [] => 50,
      [option, value] if option == "--limit" => value.parse()?,
      _ => return Err("usage: ctream-agent activity [--details] [--limit COUNT]".into()),
    };
    if details {
      activity::write_recent_details(std::io::stdout().lock(), limit)?;
    } else {
      activity::write_recent(std::io::stdout().lock(), limit)?;
    }
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
fn live_room(
  target: ctream_proto::Target,
  agent_id: &str,
  window: Option<&str>,
  turns: bool,
  authoritative: bool,
) -> ctream_driver::Result<()> {
  let participant_id = uuid::Uuid::new_v4().to_string();
  let serve = if authoritative {
    ctream_agent::daemon::serve_observer
  } else if turns {
    ctream_agent::daemon::serve_turn_peer
  } else {
    ctream_agent::daemon::serve_peer
  };
  Ok(serve(
    target,
    window,
    agent_id,
    &participant_id,
    std::io::stdin(),
    std::io::stdout().lock(),
  )?)
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn main() -> std::process::ExitCode {
  match run() {
    Ok(()) => std::process::ExitCode::SUCCESS,
    Err(error) => {
      eprintln!("ctream-agent: {error}");
      if std::env::var_os("CTREAM_INPUT_DELEGATION").is_some()
        && !error
          .downcast_ref::<std::io::Error>()
          .is_some_and(ctream_driver::UnconfirmedDelivery::is)
      {
        return std::process::ExitCode::from(ctream_driver::DELEGATED_FAILURE_EXIT);
      }
      std::process::ExitCode::FAILURE
    }
  }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn main() -> std::process::ExitCode {
  eprintln!("ctream-agent requires a Linux, macOS, or Windows host");
  std::process::ExitCode::FAILURE
}
