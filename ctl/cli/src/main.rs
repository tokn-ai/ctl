mod commands;
mod connection;
mod host;
mod openssh;
#[cfg(unix)]
mod port;
mod setup;
mod skill;
#[cfg(unix)]
mod ssh_broker;
mod table;
mod target;
mod vpn;

use clap::{Parser, Subcommand, ValueEnum};
use ctl_task_cli::Command as TaskCommand;
use ctmux_cli::Command as CtmuxCommand;

#[derive(Debug, Parser)]
#[command(
  name = "ctl",
  version,
  about = "Route control commands locally or over OpenSSH"
)]
struct Arguments {
  /// Select a saved ctl host, OpenSSH alias, or destination instead of local.
  #[arg(long, short = 'H', global = true, value_name = "DESTINATION")]
  host: Option<String>,

  /// Select a saved host's connection method by name or ID.
  #[arg(long, global = true)]
  method: Option<String>,

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
  /// Install the signed macOS ctld helper for this ctl release, without restarting it.
  Setup(setup::Arguments),
  /// Print bundled agent skills and supporting guides without connecting.
  Skill(skill::Arguments),
  /// Manage saved hosts and inspect their connection status.
  Host {
    #[command(subcommand)]
    command: host::Command,
  },
  /// Open a persistent ctmux shell (or an ordinary shell with --plain).
  Shell {
    /// Attach to this named session, creating it if absent.
    #[arg(long, short = 's', conflicts_with = "plain")]
    session: Option<String>,
    /// Open an ordinary shell without ctmux.
    #[arg(long)]
    plain: bool,
    /// Working directory for a new ctmux session.
    #[arg(long, short = 'c', conflicts_with = "plain")]
    cwd: Option<String>,
  },
  /// Run a command once, streaming input/output and preserving its exit status.
  Exec {
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
  },
  /// OpenSSH-compatible login using saved ctl hosts and managed connections.
  #[command(disable_help_flag = true, trailing_var_arg = true)]
  Ssh {
    #[arg(allow_hyphen_values = true)]
    arguments: Vec<std::ffi::OsString>,
  },
  /// OpenSSH-compatible file copy using the same host lookup as ctl ssh.
  #[command(disable_help_flag = true, trailing_var_arg = true)]
  Scp {
    #[arg(allow_hyphen_values = true)]
    arguments: Vec<std::ffi::OsString>,
  },
  /// Manage ctld-owned local port forwards.
  #[cfg(unix)]
  Port {
    #[command(subcommand)]
    command: port::Command,
  },
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
  /// Run the canonical ctmux command surface through the selected target.
  Ctmux {
    #[command(subcommand)]
    command: CtmuxCommand,
  },
  /// Manage reusable background and interactive tasks.
  Task {
    #[command(subcommand)]
    command: TaskCommand,
  },
}

#[derive(Debug, Subcommand)]
enum TaskdCommand {
  /// Restart an idle ctl-taskd, retaining saved task state. Starts it if absent.
  Restart,
}

#[tokio::main]
async fn main() {
  let result = if std::env::var_os(openssh::SCP_TRANSPORT_ENV).is_some() {
    openssh::run_ssh(
      std::env::args_os().skip(1).collect(),
      std::env::var("CTL_SCP_METHOD").ok().as_deref(),
    )
    .await
    .map_err(commands::CliError::from)
  } else {
    commands::run(Arguments::parse()).await
  };
  match result {
    Ok(code) => std::process::exit(code),
    Err(error) => {
      eprintln!("ctl: {error}");
      std::process::exit(1);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn setup_accepts_json_and_never_accepts_unsigned_source_overrides() {
    for options in [vec!["ctl", "setup"], vec!["ctl", "setup", "--json"]] {
      assert!(matches!(
        Arguments::try_parse_from(options).unwrap().command,
        Command::Setup(_)
      ));
    }
    for option in ["--url", "--version", "--unsigned", "--restart"] {
      assert!(Arguments::try_parse_from(["ctl", "setup", option]).is_err());
    }
  }

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
      "ctmux-test",
      "--accept-routes",
      "--json",
    ])
    .unwrap();
    assert!(matches!(arguments.command, Command::Vpn {
      command: vpn::Command::StartTailscale { connection_id, hostname: Some(hostname), accept_routes: true, json: true, .. }
    } if connection_id == "team" && hostname == "ctmux-test"));
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
  fn ctmux_uses_the_local_target_by_default() {
    let arguments = Arguments::try_parse_from(["ctl", "ctmux", "list"]).unwrap();
    assert_eq!(arguments.host, None);
    assert!(matches!(
      arguments.command,
      Command::Ctmux {
        command: CtmuxCommand::List
      }
    ));
  }

  #[test]
  fn host_flag_routes_the_same_ctmux_command_over_ssh() {
    let arguments = Arguments::try_parse_from([
      "ctl",
      "--host",
      "workstation",
      "ctmux",
      "attach",
      "development",
    ])
    .unwrap();
    assert_eq!(arguments.host.as_deref(), Some("workstation"));
    assert!(matches!(
      arguments.command,
      Command::Ctmux {
        command: CtmuxCommand::Attach { session, .. }
      } if session.as_deref() == Some("development")
    ));
  }

  #[test]
  fn remote_platform_requires_host_and_rejects_arbitrary_commands() {
    assert!(
      Arguments::try_parse_from(["ctl", "--remote-platform", "windows", "ctmux", "list"]).is_err()
    );
    assert!(
      Arguments::try_parse_from([
        "ctl",
        "--host",
        "server",
        "--remote-platform",
        "windows",
        "ctmux",
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
        "ctmux",
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
