#[cfg(unix)]
mod unix;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version, about = "Persistent local or remote rmux sessions")]
struct Arguments {
  /// Use an OpenSSH destination or Host alias instead of the local rmux daemon.
  #[arg(long, short = 'H', global = true, value_name = "DESTINATION")]
  host: Option<String>,

  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Manage persistent shell sessions.
  Session {
    #[command(subcommand)]
    command: SessionCommand,
  },

  /// Attach to a shell, creating the named session when it is absent.
  Shell {
    /// Session name or ID. Defaults to the named `shell` session.
    #[arg(default_value = "shell")]
    session: String,

    /// Resume at this raw terminal-output sequence.
    #[arg(long = "from")]
    resume_from: Option<u64>,

    /// Attach without requesting the input lease.
    #[arg(long)]
    read_only: bool,

    /// Request layout ownership and explicitly resize the PTY.
    #[arg(long)]
    resize: bool,
  },
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
  /// List sessions.
  List,

  /// Create a persistent shell session.
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

  /// Terminate a session by name or ID.
  Kill {
    /// Session name or ID.
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn local_shell_is_the_zero_configuration_default() {
    let arguments = Arguments::try_parse_from(["ctl", "shell"]).unwrap();
    assert_eq!(arguments.host, None);
    assert!(matches!(
      arguments.command,
      Command::Shell { session, .. } if session == "shell"
    ));
  }

  #[test]
  fn host_flag_selects_ssh_without_consuming_the_session_name() {
    let arguments =
      Arguments::try_parse_from(["ctl", "--host", "workstation", "shell", "development"]).unwrap();
    assert_eq!(arguments.host.as_deref(), Some("workstation"));
    assert!(matches!(
      arguments.command,
      Command::Shell { session, .. } if session == "development"
    ));
  }
}
