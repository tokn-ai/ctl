//! Shared command surface for the local `rmux` CLI and `ctl rmux`.

mod shell;
#[cfg(unix)]
mod unix;

use clap::{Subcommand, ValueEnum};
use rmux_proto::CommandSpec;

#[cfg(unix)]
pub use unix::{CommandError, ConnectFuture, Connector, LocalConnector, run};

/// Canonical rmux commands, independent of how the daemon is reached.
#[derive(Debug, Subcommand)]
pub enum Command {
  /// Create a persistent terminal session.
  New {
    /// Stable, human-readable session name.
    #[arg(long, short)]
    name: Option<String>,

    /// Initial working directory. Local sessions default to the current directory.
    #[arg(long)]
    cwd: Option<String>,

    /// Program and arguments. Omit to use the target's default shell.
    #[arg(last = true)]
    command: Vec<String>,
  },

  /// List running sessions.
  List,

  /// Show non-sensitive shell-awareness metadata for a running session.
  State { session: String },

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

  /// Print shell integration helpers that do not require a daemon connection.
  Shell {
    #[command(subcommand)]
    command: ShellCommand,
  },
}

#[derive(Debug, Subcommand)]
pub enum ShellCommand {
  /// Print a shell startup snippet for rmux session awareness.
  Init {
    #[arg(value_enum)]
    shell: ShellKind,
  },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ShellKind {
  Bash,
  Zsh,
}

fn command_spec(command: Vec<String>) -> Option<CommandSpec> {
  let mut command = command.into_iter();
  let program = command.next()?;
  Some(CommandSpec {
    program,
    arguments: command.collect(),
  })
}

impl From<ShellKind> for shell::Shell {
  fn from(value: ShellKind) -> Self {
    match value {
      ShellKind::Bash => Self::Bash,
      ShellKind::Zsh => Self::Zsh,
    }
  }
}
