mod relay;

use ctream_client::{
  Client, generate_agent_id, options::ConnectionOptions, sdk, transport::Transport,
};
use ctream_platform::{discover_sessions, windows};
use ctream_proto::{Display, Session, Target, Window};
use std::{env, ffi::OsString, io, process::ExitCode};

const USAGE: &str = "Usage: ctream [OPTIONS] list [--json]
       ctream [OPTIONS] windows TARGET [--json]
       ctream [OPTIONS] do TARGET [--window ID] [--protocol 1.0.1]
       ctream [OPTIONS] session TARGET [--agent-id ID] [--window ID] [--protocol 1.0.4]
       ctream [OPTIONS] watch TARGET [--agent-id ID] [--window ID] [--protocol 1.0.5]
       ctream [OPTIONS] session|watch TARGET [--agent-id ID] [--window ID] --protocol 1.0.7
       ctream [OPTIONS] session TARGET [--window ID] --protocol 1.0.2
       ctream [OPTIONS] session TARGET --agent-id ID [--window ID] [--protocol 1.0.3]

Connection options (before the command):
  --host SSH_HOST       Remote SSH endpoint
  --agent EXECUTABLE    Agent executable; use a mounted Windows path through WSL
  --ssh-program PROGRAM  SSH-compatible launcher (default: ssh)
  --ssh-arg ARG         Launcher prefix argument; repeat as needed

do reads one JSON operation from stdin and writes its JSON result to stdout.
session serves one JSON operation per line and flushes one reply per operation.
watch streams a participant roster and live action events until its stdin closes.
By default, session assigns a word#hhhh agent ID when --agent-id is omitted.
Select 1.0.2 explicitly for an anonymous session.
Operations: capabilities, screenshot, move, click, drag, scroll, press_keys,
            type_text, elements, invoke, set_text.

Use :DISPLAY for X11, wayland-N for Wayland, windows-N for one Windows display,
windows for the Windows desktop, or macos for windows in a macOS session.";

const SDK_PROTOCOL: &str = "1.0.1";
const SESSION_PROTOCOL: &str = "1.0.2";
const NAMED_PROTOCOL: &str = "1.0.3";
const GENERATED_ID_PROTOCOL: &str = "1.0.4";

const WATCH_PROTOCOL: &str = "1.0.5";
const TURN_PROTOCOL: &str = "1.0.6";
const DAEMON_PROTOCOL: &str = "1.0.7";

enum Mode {
  Watch(String),
  TurnSession(String, &'static str),
  TurnWatch(String, &'static str),
  Once,
  Session,
  Named(String),
}

enum Operation {
  List,
  Windows(Target),
  Sdk(Mode, Target, Option<String>),
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

fn sdk_mode(command: &str, protocol: Option<&str>, agent_id: Option<String>) -> io::Result<Mode> {
  match (command, protocol, agent_id) {
    ("session" | "watch", Some(wire @ (TURN_PROTOCOL | DAEMON_PROTOCOL)), id) => {
      let host = if wire == DAEMON_PROTOCOL {
        ctream_proto::DAEMON_VERSION
      } else {
        ctream_proto::TURN_VERSION
      };
      let id = id.unwrap_or_else(generate_agent_id);
      Ok(if command == "watch" {
        Mode::TurnWatch(id, host)
      } else {
        Mode::TurnSession(id, host)
      })
    }
    ("watch", None | Some(WATCH_PROTOCOL), id) => {
      Ok(Mode::Watch(id.unwrap_or_else(generate_agent_id)))
    }
    ("do", None | Some(SDK_PROTOCOL), None) => Ok(Mode::Once),
    ("session", Some(SESSION_PROTOCOL), None) => Ok(Mode::Session),
    ("session", Some(NAMED_PROTOCOL), Some(id))
      if ctream_proto::agent_id_version(&id) == Some(ctream_proto::ATTRIBUTED_SESSION_VERSION) =>
    {
      Ok(Mode::Named(id))
    }
    ("session", None | Some(GENERATED_ID_PROTOCOL), id) => {
      Ok(Mode::Named(id.unwrap_or_else(generate_agent_id)))
    }
    ("session", Some(SESSION_PROTOCOL), _) => {
      Err(invalid("ctream_sdk 1.0.2 requires an anonymous session"))
    }
    ("session", Some(NAMED_PROTOCOL), _) => {
      Err(invalid("ctream_sdk 1.0.3 requires a plain --agent-id"))
    }
    _ => Err(invalid(&format!(
      "unsupported ctream_sdk protocol for {command}"
    ))),
  }
}

fn parse(arguments: impl IntoIterator<Item = OsString>) -> io::Result<Option<Options>> {
  let mut arguments = arguments.into_iter();
  let mut connection = ConnectionOptions::default();
  let mut host = None;
  let command = loop {
    let argument = arguments
      .next()
      .ok_or_else(|| invalid("expected list, windows, do, session, or watch"))?;
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
      "list" | "windows" | "do" | "session" | "watch" => break argument.to_owned(),
      argument if connection.parse_option(argument, &mut arguments)? => {}
      _ => return Err(invalid("unexpected global option")),
    }
  };
  let mut json = false;
  let mut target = None;
  let mut window = None;
  let is_sdk = matches!(command.as_str(), "do" | "session" | "watch");
  let mut selected_protocol = None;
  let mut agent_id = None;
  while let Some(argument) = arguments.next() {
    match argument.to_str() {
      Some("--help" | "-h") => return Ok(None),
      Some("--json") if !is_sdk && !json => json = true,
      Some("--window") if is_sdk && window.is_none() => {
        let id = arguments
          .next()
          .and_then(|value| value.into_string().ok())
          .filter(|value| ctream_proto::valid_window_id(value))
          .ok_or_else(|| invalid("--window needs a window ID"))?;
        window = Some(id);
      }
      Some("--protocol") if is_sdk && selected_protocol.is_none() => {
        selected_protocol = Some(
          arguments
            .next()
            .and_then(|value| value.into_string().ok())
            .ok_or_else(|| invalid("--protocol needs a version"))?,
        );
      }
      Some("--agent-id")
        if matches!(command.as_str(), "session" | "watch") && agent_id.is_none() =>
      {
        agent_id = Some(
          arguments
            .next()
            .and_then(|value| value.into_string().ok())
            .filter(|value| ctream_proto::valid_agent_id(value))
            .ok_or_else(|| invalid("--agent-id needs a valid agent ID"))?,
        );
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
    _ => {
      let target = target.ok_or_else(|| invalid(&format!("{command} needs a target")))?;
      Operation::Sdk(
        sdk_mode(&command, selected_protocol.as_deref(), agent_id)?,
        target,
        window,
      )
    }
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
    Operation::Sdk(mode, target, window) => {
      if let Some(host) = &options.host {
        run_sdk(options.connection.ssh(host)?, mode, target, window)?;
      } else {
        run_sdk(options.connection.local()?, mode, target, window)?;
      }
    }
  }
  Ok(())
}

fn run_sdk<T: Transport>(
  transport: T,
  mode: Mode,
  target: Target,
  window: Option<String>,
) -> io::Result<()> {
  match mode {
    Mode::TurnSession(id, host_version) => {
      relay::run(transport, host_version, "turn-session", target, &id, window)
    }
    Mode::TurnWatch(id, host_version) => {
      relay::run(transport, host_version, "turn-room", target, &id, window)
    }
    Mode::Watch(agent_id) => relay::run(
      transport,
      ctream_proto::LIVE_VERSION,
      "live-room",
      target,
      &agent_id,
      window,
    ),
    Mode::Named(agent_id) => {
      let version =
        ctream_proto::agent_id_version(&agent_id).ok_or_else(|| invalid("invalid agent ID"))?;
      relay::run(
        transport,
        version,
        "attributed-session",
        target,
        &agent_id,
        window,
      )
    }
    Mode::Once => sdk::run(
      &Client::new(transport),
      target,
      window,
      io::stdin().lock(),
      io::stdout().lock(),
    ),
    Mode::Session => sdk::run_session(
      &Client::new(transport),
      target,
      window,
      io::stdin().lock(),
      io::stdout().lock(),
    ),
  }
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
      vec!["session", ":3", "--json"],
      vec!["session", ":3", "--protocol", NAMED_PROTOCOL],
      vec![
        "session",
        ":3",
        "--protocol",
        NAMED_PROTOCOL,
        "--agent-id",
        "fern#abcd",
      ],
      vec![
        "session",
        ":3",
        "--protocol",
        GENERATED_ID_PROTOCOL,
        "--agent-id",
        "Fern#abcd",
      ],
      vec!["session", ":3", "--agent-id", "fern#ABCD"],
      vec![
        "session",
        ":3",
        "--agent-id",
        "worker",
        "--protocol",
        SESSION_PROTOCOL,
      ],
      vec!["session", ":3", "--agent-id", "bad\nlabel"],
      vec!["session", ":3", "--agent-id", "one", "--agent-id", "two"],
      vec!["do", ":3", "--agent-id", "worker"],
      vec!["do", ":3", "--protocol", SESSION_PROTOCOL],
      vec!["session", ":3", "--protocol", SDK_PROTOCOL],
      vec!["do", ":3", ":4"],
      vec!["windows", ":3", "--window", "x11-4"],
    ] {
      assert!(parse(args(&arguments)).is_err(), "{arguments:?}");
    }
  }

  #[test]
  fn unversioned_and_current_sessions_generate_a_tag_or_preserve_the_explicit_id() {
    for flags in [&[][..], &["--protocol", GENERATED_ID_PROTOCOL][..]] {
      let mut command = vec!["session", ":1"];
      command.extend_from_slice(flags);
      let options = parse(args(&command)).unwrap().unwrap();
      let Operation::Sdk(Mode::Named(id), Target::X11(1), None) = options.operation else {
        panic!("expected a named session");
      };
      let words: [&str; 32] =
        serde_json::from_str(include_str!("../../sdk/src/agent-words.json")).unwrap();
      assert!(words.contains(&id.split_once('#').unwrap().0));
      assert_eq!(
        ctream_proto::agent_id_version(&id),
        Some(ctream_proto::TAGGED_AGENT_ID_VERSION)
      );

      for explicit in ["Worker.xyz", "custom#000a"] {
        let mut named = command.clone();
        named.extend_from_slice(&["--agent-id", explicit]);
        let options = parse(args(&named)).unwrap().unwrap();
        assert!(matches!(options.operation, Operation::Sdk(Mode::Named(id), ..) if id == explicit));
      }
    }
  }

  #[test]
  fn explicit_retained_contracts_keep_their_anonymous_or_caller_named_semantics() {
    let anonymous = parse(args(&["session", ":1", "--protocol", SESSION_PROTOCOL]))
      .unwrap()
      .unwrap();
    assert!(matches!(
      anonymous.operation,
      Operation::Sdk(Mode::Session, ..)
    ));
    let named = parse(args(&[
      "session",
      ":1",
      "--protocol",
      NAMED_PROTOCOL,
      "--agent-id",
      "Worker_1",
    ]))
    .unwrap()
    .unwrap();
    assert!(matches!(named.operation, Operation::Sdk(Mode::Named(id), ..) if id == "Worker_1"));
    let once = parse(args(&["do", ":1"])).unwrap().unwrap();
    assert!(matches!(once.operation, Operation::Sdk(Mode::Once, ..)));
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
      Operation::Sdk(Mode::Once, Target::X11(3), Some(id)) if id == "x11-4"
    ));
  }
}
