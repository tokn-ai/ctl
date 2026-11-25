mod operation;

use ctream_client::{Client, options::ConnectionOptions};
use ctream_platform::{discover_sessions, windows};
use ctream_proto::{Display, Session, Target, Window};
use std::{env, ffi::OsString, io, process::ExitCode};

const USAGE: &str = "Usage: ctream [OPTIONS] list [--json]
       ctream [OPTIONS] windows TARGET [--json]
       ctream [OPTIONS] do TARGET [--window ID] [--protocol 1.0.1]

Connection options (before the command):
  --host SSH_HOST       Remote SSH endpoint
  --agent EXECUTABLE    Agent executable; use a mounted Windows path through WSL
  --ssh-program PROGRAM  SSH-compatible launcher (default: ssh)
  --ssh-arg ARG         Launcher prefix argument; repeat as needed

do reads one JSON operation from stdin and writes its JSON result to stdout.
Operations: capabilities, screenshot, move, click, drag, scroll, press_keys,
            type_text, elements, invoke, set_text.

Use :DISPLAY for X11, wayland-N for Wayland, windows-N for one Windows display,
windows for the Windows desktop, or macos for windows in a macOS session.";

const SDK_PROTOCOL: &str = "1.0.1";

enum Operation {
  List,
  Windows(Target),
  Do(Target, Option<String>),
}

struct Options {
  connection: ConnectionOptions,
  host: Option<String>,
  operation: Operation,
  json: bool,
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn parse(arguments: impl IntoIterator<Item = OsString>) -> io::Result<Option<Options>> {
  let mut arguments = arguments.into_iter();
  let mut connection = ConnectionOptions::default();
  let mut host = None;
  let command = loop {
    let argument = arguments
      .next()
      .ok_or_else(|| invalid("expected list, windows, or do"))?;
    let argument = argument
      .to_str()
      .ok_or_else(|| invalid("invalid command argument"))?;
    match argument {
      "--help" | "-h" => return Ok(None),
      "--host" if host.is_none() => {
        let value = arguments
          .next()
          .ok_or_else(|| invalid("--host needs an SSH destination"))?;
        let value = value
          .to_str()
          .filter(|value| !value.is_empty() && !value.starts_with('-'))
          .ok_or_else(|| invalid("--host needs an SSH destination"))?;
        host = Some(value.to_owned());
      }
      "list" | "windows" | "do" => break argument.to_owned(),
      argument if connection.parse_option(argument, &mut arguments)? => {}
      _ => return Err(invalid("unexpected global option")),
    }
  };
  let mut json = false;
  let mut target = None;
  let mut window = None;
  let mut protocol_selected = false;
  while let Some(argument) = arguments.next() {
    match argument.to_str() {
      Some("--help" | "-h") => return Ok(None),
      Some("--json") if command != "do" && !json => json = true,
      Some("--window") if command == "do" && window.is_none() => {
        let id = arguments
          .next()
          .and_then(|value| value.into_string().ok())
          .filter(|value| ctream_proto::valid_window_id(value))
          .ok_or_else(|| invalid("--window needs a window ID"))?;
        window = Some(id);
      }
      Some("--protocol") if command == "do" && !protocol_selected => {
        if arguments.next().as_deref() != Some(SDK_PROTOCOL.as_ref()) {
          return Err(invalid(&format!(
            "supported ctream_sdk protocol is {SDK_PROTOCOL}"
          )));
        }
        protocol_selected = true;
      }
      Some("--host" | "--agent" | "--ssh-program" | "--ssh-arg") => {
        return Err(invalid("connection options must appear before the command"));
      }
      Some(value) if command != "list" && target.is_none() && !value.starts_with('-') => {
        target = Some(Target::parse(value).map_err(invalid)?);
      }
      _ => return Err(invalid("unexpected command argument")),
    }
  }
  let operation = match command.as_str() {
    "list" => Operation::List,
    "windows" => Operation::Windows(target.ok_or_else(|| invalid("windows needs a target"))?),
    _ => Operation::Do(target.ok_or_else(|| invalid("do needs a target"))?, window),
  };
  if host.is_none() {
    connection.local()?;
  }
  Ok(Some(Options {
    connection,
    host,
    operation,
    json,
  }))
}

fn one_line(text: &str) -> String {
  text
    .chars()
    .map(|ch| if ch.is_control() { ' ' } else { ch })
    .collect()
}

fn print_displays(sessions: Vec<Session>, json: bool) -> io::Result<()> {
  if json {
    println!("{}", serde_json::to_string(&sessions)?);
    return Ok(());
  }
  println!("PROTOCOL\tDISPLAY\tUID\tSESSION CLASS\tDETAILS");
  for session in sessions {
    let class = one_line(session.class.as_deref().unwrap_or("-"));
    match session.display {
      Display::X11(number) => println!("X11\t:{number}\t-\t{class}\t-"),
      Display::Wayland { uid, socket } => {
        println!("Wayland\t{}\t{uid}\t{class}\t-", one_line(&socket))
      }
      Display::Windows {
        number,
        name,
        bounds,
      } => println!(
        "Windows\twindows-{number}\t-\t{class}\t{} · {}x{} @ {},{}",
        one_line(&name),
        i64::from(bounds[2]) - i64::from(bounds[0]),
        i64::from(bounds[3]) - i64::from(bounds[1]),
        bounds[0],
        bounds[1],
      ),
      Display::MacosDesktop => println!("macOS\tmacos\t-\t{class}\tWindows"),
    }
  }
  Ok(())
}

fn print_windows(windows: Vec<Window>, json: bool) -> io::Result<()> {
  if json {
    println!("{}", serde_json::to_string(&windows)?);
  } else {
    println!("ID\tAPP ID\tTITLE");
    for window in windows {
      println!(
        "{}\t{}\t{}",
        one_line(&window.id),
        one_line(&window.app_id),
        one_line(&window.title)
      );
    }
  }
  Ok(())
}

fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
  match options.operation {
    Operation::List => {
      let sessions = if let Some(host) = &options.host {
        Client::new(options.connection.ssh(host)?).discover()?
      } else if options.connection.agent_is_explicit() || cfg!(target_os = "macos") {
        Client::new(options.connection.local()?).discover()?
      } else {
        discover_sessions()?
      };
      print_displays(sessions, options.json)?;
    }
    Operation::Windows(target) => {
      let windows = if let Some(host) = &options.host {
        Client::new(options.connection.ssh(host)?).windows(target)?
      } else if options.connection.agent_is_explicit() || cfg!(target_os = "macos") {
        Client::new(options.connection.local()?).windows(target)?
      } else {
        windows::list(target)?
      };
      print_windows(windows, options.json)?;
    }
    Operation::Do(target, window) => {
      let input = io::stdin().lock();
      let output = io::stdout().lock();
      if let Some(host) = &options.host {
        operation::run(
          &Client::new(options.connection.ssh(host)?),
          target,
          window,
          input,
          output,
        )?;
      } else {
        operation::run(
          &Client::new(options.connection.local()?),
          target,
          window,
          input,
          output,
        )?;
      }
    }
  }
  Ok(())
}

fn main() -> ExitCode {
  let options = match parse(env::args_os().skip(1)) {
    Ok(Some(options)) => options,
    Ok(None) => {
      println!("{USAGE}");
      return ExitCode::SUCCESS;
    }
    Err(error) => {
      eprintln!("ctream: {error}\n{USAGE}");
      return ExitCode::from(2);
    }
  };
  match run(options) {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      eprintln!("ctream: {error}");
      ExitCode::FAILURE
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(arguments: &[&str]) -> Vec<OsString> {
    arguments.iter().map(OsString::from).collect()
  }

  #[test]
  fn remote_discovery_accepts_launcher_options_before_the_command() {
    let options = parse(args(&[
      "--host",
      "test-host",
      "--ssh-program",
      "ctl",
      "--ssh-arg",
      "ssh",
      "--agent",
      "/mounted drive/agent.exe",
      "list",
      "--json",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(options.host.as_deref(), Some("test-host"));
    assert!(options.json);
    assert!(matches!(options.operation, Operation::List));
    assert!(options.connection.agent_is_explicit());
  }

  #[test]
  fn connection_options_after_command_and_invalid_combinations_are_rejected() {
    for arguments in [
      vec!["windows", ":1", "--host", "test-host"],
      vec!["list", "--agent", "agent"],
      vec!["--ssh-program", "ctl", "list"],
      vec!["--host", "test-host", "--host", "another", "list"],
      vec!["list", "windows-1"],
      vec!["windows"],
      vec!["do", ":3", "--protocol", "9.0.1"],
      vec!["do", ":3", "--protocol"],
      vec!["do", ":3", "--window"],
      vec!["do", ":3", "--window", "x11-4", "--window", "x11-5"],
      vec!["do", ":3", "--json"],
      vec!["do", ":3", ":4"],
      vec!["windows", ":3", "--window", "x11-4"],
    ] {
      assert!(parse(args(&arguments)).is_err(), "{arguments:?}");
    }
  }

  #[test]
  fn sdk_selects_an_explicit_window_and_local_contract() {
    let options = parse(args(&[
      "--host",
      "fixture",
      "do",
      ":3",
      "--protocol",
      SDK_PROTOCOL,
      "--window",
      "x11-4",
    ]))
    .unwrap()
    .unwrap();
    assert!(matches!(
      options.operation,
      Operation::Do(Target::X11(3), Some(id)) if id == "x11-4"
    ));
  }
}
