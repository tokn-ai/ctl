//! Shared command surface for the local `rmux` CLI and `ctl rmux`.

mod commands;
mod shell;

use clap::{Subcommand, ValueEnum};
use rmux_proto::CommandSpec;

pub use commands::{
  CommandError, ConnectFuture, Connector, LocalConnector, new_session, resolve_session, run,
};

/// Canonical rmux commands, independent of how the daemon is reached.
#[derive(Debug, Subcommand)]
pub enum Command {
  /// Create a persistent terminal session.
  #[command(name = "new-session", visible_alias = "new")]
  New {
    /// Stable, human-readable session name.
    #[arg(long, short = 's', visible_short_alias = 'n')]
    name: Option<String>,

    /// Create without opening a terminal UI.
    #[arg(short = 'd', long)]
    detached: bool,

    /// Attach to the named session if it already exists.
    #[arg(short = 'A', long, requires = "name")]
    attach_if_exists: bool,

    /// Initial working directory. Local sessions default to the current directory.
    #[arg(long, short = 'c')]
    cwd: Option<String>,

    /// Program and arguments. Omit to use the target's default shell.
    #[arg(last = true)]
    command: Vec<String>,
  },

  /// List running sessions.
  #[command(name = "list-sessions", visible_aliases = ["ls", "list"])]
  List,

  /// Inspect the server-owned layout and terminal IDs.
  View { session: String },

  /// Split a terminal, creating another terminal in the same view.
  Split {
    terminal_id: String,
    /// Stack panes vertically instead of side by side.
    #[arg(long)]
    vertical: bool,
    #[arg(long)]
    cwd: Option<String>,
    #[arg(last = true)]
    command: Vec<String>,
  },

  /// Move a terminal into a new top-level session.
  Promote {
    terminal_id: String,
    #[arg(long)]
    name: Option<String>,
  },

  /// Join the source and destination layouts in a horizontal split.
  Merge { source: String, destination: String },

  /// Terminate one terminal, keeping its siblings alive.
  KillTerminal { terminal_id: String },

  /// Show non-sensitive shell-awareness metadata for a running session.
  State { session: String },

  /// Attach to a session by name or ID.
  #[command(name = "attach-session", visible_aliases = ["attach", "a"])]
  Attach {
    /// Legacy positional target.
    #[arg(conflicts_with = "target")]
    session: Option<String>,

    /// Session name or ID; defaults to the newest running session.
    #[arg(short = 't', long)]
    target: Option<String>,

    /// Use the original single-terminal presenter instead of the TUI.
    #[arg(long)]
    raw: bool,

    /// Resume at this raw output byte sequence.
    #[arg(long = "from")]
    resume_from: Option<u64>,

    /// Attach without requesting the input lease.
    #[arg(long, short = 'r')]
    read_only: bool,

    /// Request layout ownership and explicitly resize the PTY to this terminal.
    #[arg(long)]
    resize: bool,
  },

  /// Terminate a session by name or ID.
  #[command(name = "kill-session", visible_alias = "kill")]
  Kill {
    #[arg(required_unless_present = "target", conflicts_with = "target")]
    session: Option<String>,
    #[arg(short = 't', long)]
    target: Option<String>,
  },

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
