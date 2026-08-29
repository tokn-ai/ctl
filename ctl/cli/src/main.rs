#[cfg(unix)]
mod unix;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(version, about = "Remote control client for persistent rmux sessions")]
struct Arguments {
  /// Override the private directory containing this client's identity and hosts.
  #[arg(long, global = true, value_name = "DIR")]
  state_dir: Option<PathBuf>,

  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Authorize this client with a one-time invitation from ctld.
  Pair {
    /// Encoded pairing invitation from `ctld pair create`. Omit it to read one line from standard input.
    invitation: Option<String>,

    /// Local name for the paired device.
    #[arg(long)]
    alias: String,
  },

  /// List paired remote devices.
  Hosts,

  /// Manage remote persistent shell sessions.
  Session {
    #[command(subcommand)]
    command: SessionCommand,
  },

  /// Attach to a remote shell, creating a named shell when it is absent.
  Shell {
    /// Local alias of the paired remote device.
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
  /// List sessions on a remote device.
  List {
    /// Local alias of the paired remote device.
    host: String,
  },

  /// Create a persistent remote shell session.
  New {
    /// Local alias of the paired remote device.
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
    /// Local alias of the paired remote device.
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
  eprintln!("ctl: local client-state storage is not yet implemented on this platform");
  std::process::exit(1);
}
