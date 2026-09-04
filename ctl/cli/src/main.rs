mod commands;

use clap::{Parser, Subcommand};
use rmux_cli::Command as RmuxCommand;
use task_cli::Command as TaskCommand;

#[derive(Debug, Parser)]
#[command(version, about = "Route control commands locally or over OpenSSH")]
struct Arguments {
  /// Use an OpenSSH destination or Host alias instead of the local target.
  #[arg(long, short = 'H', global = true, value_name = "DESTINATION")]
  host: Option<String>,

  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Run the canonical rmux command surface through the selected target.
  Rmux {
    #[command(subcommand)]
    command: RmuxCommand,
  },
  /// Manage reusable background and interactive tasks.
  Task {
    #[command(subcommand)]
    command: TaskCommand,
  },
}

#[tokio::main]
async fn main() {
  let arguments = Arguments::parse();
  if let Err(error) = commands::run(arguments).await {
    eprintln!("ctl: {error}");
    std::process::exit(1);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn rmux_uses_the_local_target_by_default() {
    let arguments = Arguments::try_parse_from(["ctl", "rmux", "list"]).unwrap();
    assert_eq!(arguments.host, None);
    assert!(matches!(
      arguments.command,
      Command::Rmux {
        command: RmuxCommand::List
      }
    ));
  }

  #[test]
  fn host_flag_routes_the_same_rmux_command_over_ssh() {
    let arguments = Arguments::try_parse_from([
      "ctl",
      "--host",
      "workstation",
      "rmux",
      "attach",
      "development",
    ])
    .unwrap();
    assert_eq!(arguments.host.as_deref(), Some("workstation"));
    assert!(matches!(
      arguments.command,
      Command::Rmux {
        command: RmuxCommand::Attach { session, .. }
      } if session == "development"
    ));
  }

  #[test]
  fn task_commands_use_the_ctl_command_surface() {
    let arguments = Arguments::try_parse_from(["ctl", "task", "list"]).unwrap();
    assert_eq!(arguments.host, None);
    assert!(matches!(
      arguments.command,
      Command::Task {
        command: TaskCommand::List
      }
    ));
  }
}
