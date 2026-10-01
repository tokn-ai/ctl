use clap::Parser;
use ctmux_cli::{Command, LocalConnector};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
  name = "ctmux",
  version,
  about = "Persistent terminal sessions with a tmux-style interface"
)]
struct Arguments {
  /// Override the local endpoint (Unix socket or Windows named pipe).
  #[arg(long, short = 'S', global = true)]
  socket: Option<PathBuf>,

  /// TUI command prefix: Ctrl+letter or Alt+letter.
  #[arg(long, global = true, default_value = "Ctrl+b", value_parser = ctmux_tui::validate_prefix)]
  prefix: String,

  #[command(subcommand)]
  command: Option<Command>,
}

#[tokio::main]
async fn main() {
  if let Err(error) = run(Arguments::parse()).await {
    eprintln!("ctmux: {error}");
    std::process::exit(1);
  }
}

async fn run(arguments: Arguments) -> ctmux_tui::Result<()> {
  let socket = arguments.socket.unwrap_or_else(ctmux_ipc::socket_path);
  let connector = LocalConnector::new(socket.clone());
  let command = arguments.command.unwrap_or(Command::New {
    name: None,
    cwd: None,
    command: Vec::new(),
    detached: false,
    attach_if_exists: false,
  });
  if let Command::Archive { session_id } = command {
    return ctmux_tui::run(ctmux_tui::Options {
      socket,
      archive: Some(session_id),
      session: None,
      read_only: true,
      prefix: arguments.prefix,
    })
    .await;
  }
  let (session, read_only) = match command {
    Command::New {
      name,
      cwd,
      command,
      detached,
      attach_if_exists,
    } => {
      if !detached {
        ctmux_tui::ensure_terminal()?;
      }
      let session =
        ctmux_cli::new_session(&connector, name, command, cwd, attach_if_exists).await?;
      if detached {
        println!("{}\t{}", session.session_id, session.name);
        return Ok(());
      }
      (session.session_id, false)
    }
    Command::Attach {
      session,
      target,
      raw: false,
      resume_from: None,
      read_only,
      ..
    } => {
      ctmux_tui::ensure_terminal()?;
      (
        ctmux_cli::resolve_session(&connector, target.or(session)).await?,
        read_only,
      )
    }
    command => {
      ctmux_cli::run(command, &connector).await?;
      return Ok(());
    }
  };
  ctmux_tui::run(ctmux_tui::Options {
    archive: None,
    socket,
    session: Some(session),
    read_only,
    prefix: arguments.prefix,
  })
  .await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tmux_new_forms_and_legacy_name_flags_parse() {
    for command in ["new", "new-session"] {
      let args =
        Arguments::try_parse_from(["ctmux", command, "-Ads", "work", "-c", "/tmp"]).unwrap();
      assert!(matches!(args.command, Some(Command::New {
        name: Some(name), detached: true, attach_if_exists: true, cwd: Some(cwd), ..
      }) if name == "work" && cwd == "/tmp"));
    }
    assert!(Arguments::try_parse_from(["ctmux", "new", "-A"]).is_err());
    assert!(Arguments::try_parse_from(["ctmux", "new", "-n", "legacy"]).is_ok());
    assert!(Arguments::try_parse_from(["ctmux", "new", "--prefix", "bad"]).is_err());
    assert!(
      Arguments::try_parse_from(["ctmux"])
        .unwrap()
        .command
        .is_none()
    );
  }

  #[test]
  fn attach_list_and_kill_aliases_accept_tmux_targets() {
    for command in ["attach", "attach-session", "a"] {
      let args = Arguments::try_parse_from(["ctmux", command, "-rt", "work"]).unwrap();
      assert!(
        matches!(args.command, Some(Command::Attach { target: Some(target), read_only: true, .. }) if target == "work")
      );
    }
    for command in ["ls", "list", "list-sessions"] {
      assert!(matches!(
        Arguments::try_parse_from(["ctmux", command])
          .unwrap()
          .command,
        Some(Command::List)
      ));
    }
    assert!(Arguments::try_parse_from(["ctmux", "attach", "work", "-t", "other"]).is_err());
    assert!(Arguments::try_parse_from(["ctmux", "kill-session"]).is_err());
    assert!(
      matches!(Arguments::try_parse_from(["ctmux", "kill-session", "-t", "work"]).unwrap().command,
      Some(Command::Kill { target: Some(target), .. }) if target == "work")
    );
    assert!(
      matches!(Arguments::try_parse_from(["ctmux", "attach", "work", "--raw"]).unwrap().command,
      Some(Command::Attach { session: Some(session), raw: true, .. }) if session == "work")
    );
  }
}
