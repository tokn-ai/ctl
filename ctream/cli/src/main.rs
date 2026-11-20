use ctream_client::{Client, transport::Ssh};
use ctream_platform::{discover, windows};
use ctream_proto::{Display, Target};
use std::{env, ffi::OsString, io, process::ExitCode};

const USAGE: &str = "Usage: ctream list
       ctream [--host SSH_HOST] windows DISPLAY [--json]

List listening displays or query a single display's top-level windows.
Use :DISPLAY for X11 or wayland-N for Wayland.";

fn list_displays() -> io::Result<()> {
  let displays = discover()
    .map_err(|error| io::Error::other(format!("cannot discover Linux display sockets: {error}")))?;
  println!("PROTOCOL\tDISPLAY\tUID\tSESSION CLASS");
  for (display, class) in displays {
    match display {
      Display::X11(number) => println!("X11\t:{number}\t-\t-"),
      Display::Wayland { uid, socket } => println!(
        "Wayland\t{socket}\t{uid}\t{}",
        class.as_deref().unwrap_or("unknown")
      ),
    }
  }
  Ok(())
}

fn one_line(text: &str) -> String {
  text
    .chars()
    .map(|ch| if ch.is_control() { ' ' } else { ch })
    .collect()
}

fn list_windows(
  target: Target,
  host: Option<&str>,
  json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
  let windows = if let Some(host) = host {
    Client::new(Ssh(host.to_owned())).windows(target)?
  } else {
    windows::list(target)?
  };
  if json {
    println!("{}", serde_json::to_string(&windows)?);
  } else {
    println!("ID\tAPP ID\tTITLE");
    for window in windows {
      println!(
        "{}\t{}\t{}",
        window.id,
        one_line(&window.app_id),
        one_line(&window.title)
      );
    }
  }
  Ok(())
}

fn usage_error(error: &str) -> ExitCode {
  eprintln!("ctream: {error}\n{USAGE}");
  ExitCode::from(2)
}

fn run_windows(host: Option<&str>, args: &[OsString]) -> ExitCode {
  let mut target = None;
  let mut json = false;
  for arg in args {
    match arg.to_str() {
      Some("--help" | "-h") => {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
      }
      Some("--json") if !json => json = true,
      Some("--host") => return usage_error("--host must appear before windows"),
      Some(display) if target.is_none() && !display.starts_with('-') => {
        match Target::parse(display) {
          Ok(display) => target = Some(display),
          Err(error) => return usage_error(error),
        }
      }
      _ => return usage_error("unexpected windows argument"),
    }
  }
  let Some(target) = target else {
    return usage_error("windows needs a display");
  };
  match list_windows(target, host, json) {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      eprintln!("ctream: cannot query windows on {target}: {error}");
      ExitCode::FAILURE
    }
  }
}

fn main() -> ExitCode {
  let args: Vec<_> = env::args_os().skip(1).collect();
  match args.as_slice() {
    [arg] if arg == "--help" || arg == "-h" => {
      println!("{USAGE}");
      ExitCode::SUCCESS
    }
    [arg] if arg == "list" => match list_displays() {
      Ok(()) => ExitCode::SUCCESS,
      Err(error) => {
        eprintln!("ctream: {error}");
        ExitCode::FAILURE
      }
    },
    [arg, rest @ ..] if arg == "windows" => run_windows(None, rest),
    [flag, host, command, rest @ ..] if flag == "--host" && command == "windows" => {
      let Some(host) = host
        .to_str()
        .filter(|host| !host.is_empty() && !host.starts_with('-'))
      else {
        return usage_error("--host needs an SSH destination");
      };
      run_windows(Some(host), rest)
    }
    [flag, ..] if flag == "--host" => usage_error("expected --host SSH_HOST windows"),
    _ => usage_error("expected list or windows"),
  }
}
