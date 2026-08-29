#[cfg(unix)]
mod unix;

use clap::{Parser, Subcommand};
use rmux_proto::CommandSpec;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(version, about = "Persistent local terminal sessions")]
struct Arguments {
  /// Override the local Unix socket path.
  #[arg(long, global = true)]
  socket: Option<PathBuf>,

  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Create a persistent terminal session.
  New {
    /// Stable, human-readable session name.
    #[arg(long, short)]
    name: Option<String>,

    /// Initial working directory for an explicit command.
    #[arg(long)]
    cwd: Option<String>,

    /// Program and arguments. Omit to use the default shell.
    #[arg(last = true)]
    command: Vec<String>,
  },

  /// List running sessions.
  List,

  /// Attach to a session by name or ID.
  Attach {
    session: String,

    /// Resume at this raw output byte sequence.
    #[arg(long = "from")]
    resume_from: Option<u64>,

    /// Attach without requesting the input lease.
    #[arg(long)]
    read_only: bool,

    /// Request layout ownership and explicitly resize the PTY to this terminal.
    #[arg(long)]
    resize: bool,
  },

  /// Terminate a session by name or ID.
  Kill { session: String },
}

#[cfg(unix)]
#[tokio::main]
async fn main() {
  let arguments = Arguments::parse();
  let socket_path = arguments.socket.unwrap_or_else(rmux_ipc::socket_path);
  let result = match arguments.command {
    Command::New { name, cwd, command } => {
      let command = command_spec(command);
      unix::create_session(&socket_path, name, command, cwd).await
    }
    Command::List => unix::list_sessions(&socket_path).await,
    Command::Attach {
      session,
      resume_from,
      read_only,
      resize,
    } => unix::attach_session(&socket_path, &session, resume_from, !read_only, resize).await,
    Command::Kill { session } => unix::kill_session(&socket_path, &session).await,
  };

  if let Err(error) = result {
    eprintln!("rmux: {error}");
    std::process::exit(1);
  }
}

#[cfg(not(unix))]
fn main() {
  eprintln!("rmux: local IPC is not yet implemented on this platform");
  std::process::exit(1);
}

fn command_spec(command: Vec<String>) -> Option<CommandSpec> {
  let mut command = command.into_iter();
  let program = command.next()?;
  Some(CommandSpec {
    program,
    arguments: command.collect(),
  })
}
