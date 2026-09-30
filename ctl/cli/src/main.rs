mod commands;
#[cfg(unix)]
mod ssh_broker;
mod vpn;

use clap::{Parser, Subcommand, ValueEnum};
use rmux_cli::Command as RmuxCommand;
use task_cli::Command as TaskCommand;

#[derive(Debug, Parser)]
#[command(version, about = "Route control commands locally or over OpenSSH")]
struct Arguments {
  /// Use an OpenSSH destination or Host alias instead of the local target.
  #[arg(long, short = 'H', global = true, value_name = "DESTINATION")]
  host: Option<String>,

  /// Remote server platform (Windows currently requires the cmd.exe SSH shell).
  #[arg(long, global = true, requires = "host", value_enum)]
  remote_platform: Option<RemotePlatform>,

  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RemotePlatform {
  Unix,
  Windows,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Manage the local VPN and its SOCKS5 proxy through ctld.
  Vpn {
    #[command(subcommand)]
    command: vpn::Command,
  },
  /// Control the local task daemon.
  Taskd {
    #[command(subcommand)]
    command: TaskdCommand,
  },
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

#[derive(Debug, Subcommand)]
enum TaskdCommand {
  /// Restart an idle taskd, retaining saved task state. Starts it if absent.
  Restart,
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
  fn vpn_start_uses_a_local_env_file_and_exposes_status_and_stop() {
    let arguments = Arguments::try_parse_from(["ctl", "vpn", "start"]).unwrap();
    assert_eq!(arguments.host, None);
    assert!(matches!(
      arguments.command,
      Command::Vpn { command: vpn::Command::Start { env_file, json: false } }
        if env_file == std::path::Path::new(".env")
    ));
    let arguments =
      Arguments::try_parse_from(["ctl", "vpn", "start", "--env-file", "work.env"]).unwrap();
    assert!(matches!(
      arguments.command,
      Command::Vpn { command: vpn::Command::Start { env_file, json: false } }
        if env_file == std::path::Path::new("work.env")
    ));
    for action in ["status", "stop"] {
      assert!(Arguments::try_parse_from(["ctl", "vpn", action]).is_ok());
      assert!(Arguments::try_parse_from(["ctl", "vpn", action, "--env-file", ".env"]).is_err());
    }
    for action in ["start", "status", "stop"] {
      let arguments = Arguments::try_parse_from(["ctl", "vpn", action, "--json"]).unwrap();
      assert!(matches!(
        arguments.command,
        Command::Vpn {
          command: vpn::Command::Start { json: true, .. }
            | vpn::Command::Status { json: true }
            | vpn::Command::Stop { json: true, .. }
        }
      ));
    }
    let arguments = Arguments::try_parse_from(["ctl", "vpn", "stop", "test-vpn"]).unwrap();
    assert!(matches!(
      arguments.command,
      Command::Vpn { command: vpn::Command::Stop { vpn_id: Some(vpn_id), json: false } }
        if vpn_id == "test-vpn"
    ));
  }

  #[test]
  fn tailscale_start_requires_stable_id_and_exposes_only_supported_options() {
    assert!(Arguments::try_parse_from(["ctl", "vpn", "start-tailscale"]).is_err());
    let arguments = Arguments::try_parse_from([
      "ctl",
      "vpn",
      "start-tailscale",
      "--id",
      "team",
      "--hostname",
      "rmux-test",
      "--accept-routes",
      "--json",
    ])
    .unwrap();
    assert!(matches!(arguments.command, Command::Vpn {
      command: vpn::Command::StartTailscale { connection_id, hostname: Some(hostname), accept_routes: true, json: true, .. }
    } if connection_id == "team" && hostname == "rmux-test"));
    assert!(
      Arguments::try_parse_from([
        "ctl",
        "vpn",
        "start-tailscale",
        "--id",
        "team",
        "--auth-key",
        "secret"
      ])
      .is_err()
    );
  }

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
      } if session.as_deref() == Some("development")
    ));
  }

  #[test]
  fn remote_platform_requires_host_and_rejects_arbitrary_commands() {
    assert!(
      Arguments::try_parse_from(["ctl", "--remote-platform", "windows", "rmux", "list"]).is_err()
    );
    assert!(
      Arguments::try_parse_from([
        "ctl",
        "--host",
        "server",
        "--remote-platform",
        "windows",
        "rmux",
        "list"
      ])
      .is_ok()
    );
    assert!(
      Arguments::try_parse_from([
        "ctl",
        "--host",
        "server",
        "--remote-platform",
        "custom command",
        "rmux",
        "list"
      ])
      .is_err()
    );
  }

  #[test]
  fn taskd_restart_is_separate_from_task_restart() {
    let arguments = Arguments::try_parse_from(["ctl", "taskd", "restart"]).unwrap();
    assert!(matches!(
      arguments.command,
      Command::Taskd {
        command: TaskdCommand::Restart
      }
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

  #[test]
  fn every_task_command_accepts_the_host_and_remote_platform_options() {
    let commands: &[&[&str]] = &[
      &["create", "build", "--start", "--", "cargo", "build"],
      &["attach", "build"],
      &["list"],
      &["show", "build"],
      &["start", "build"],
      &["stop", "build"],
      &["restart", "build"],
      &["logs", "build", "--follow", "--after", "12"],
      &["remove", "build"],
    ];
    for command in commands {
      let arguments = Arguments::try_parse_from(
        [
          "ctl",
          "--host",
          "task-server",
          "--remote-platform",
          "windows",
          "task",
        ]
        .into_iter()
        .chain(command.iter().copied()),
      )
      .unwrap();
      assert_eq!(arguments.host.as_deref(), Some("task-server"));
      assert!(matches!(
        arguments.remote_platform,
        Some(RemotePlatform::Windows)
      ));
      assert!(matches!(arguments.command, Command::Task { .. }));
    }
  }
}
