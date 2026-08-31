#[cfg(unix)]
mod unix;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version, about = "Remote rmux sessions over OpenSSH")]
struct Arguments {
  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Manage persistent shell sessions on an SSH host.
  Session {
    #[command(subcommand)]
    command: SessionCommand,
  },

  /// Attach to a remote shell, creating a named shell when it is absent.
  Shell {
    /// OpenSSH destination or Host alias.
    host: String,

    /// Remote session name or ID. Defaults to the named `shell` session.
    #[arg(default_value = "shell")]
    session: String,

    /// Resume at this raw terminal-output sequence.
    #[arg(long = "from")]
    resume_from: Option<u64>,

    /// Attach without requesting the input lease.
    #[arg(long)]
    read_only: bool,

    /// Request layout ownership and explicitly resize the remote PTY.
    #[arg(long)]
    resize: bool,
  },
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
  /// List sessions on an SSH host.
  List {
    /// OpenSSH destination or Host alias.
    host: String,
  },

  /// Create a persistent remote shell session.
  New {
    /// OpenSSH destination or Host alias.
    host: String,

    /// Stable, human-readable session name.
    #[arg(long, short)]
    name: Option<String>,

    /// Initial remote working directory for an explicit command.
    #[arg(long)]
    cwd: Option<String>,

    /// Program and arguments. Omit to use the remote default shell.
    #[arg(last = true)]
    command: Vec<String>,
  },

  /// Terminate a remote session by name or ID.
  Kill {
    /// OpenSSH destination or Host alias.
    host: String,

    /// Remote session name or ID.
    session: String,
  },
}

#[cfg(unix)]
#[tokio::main]
async fn main() {
  let arguments = Arguments::parse();
  if let Err(error) = unix::run(arguments).await {
    eprintln!("ctl: {error}");
    std::process::exit(1);
  }
}

#[cfg(not(unix))]
fn main() {
  eprintln!("ctl: interactive terminal support is not yet implemented on this platform");
  std::process::exit(1);
}
