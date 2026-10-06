mod bundled;
mod commands;
mod connection;
mod host;
mod openssh;
mod passwords;
#[cfg(unix)]
mod port;
mod remote;
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
  /// Install and verify this CLI's signed macOS ctld helper, without restarting it.
  Setup(setup::Arguments),
  /// Print bundled agent skills and supporting guides without connecting.
  Skill(skill::Arguments),
  /// Manage saved hosts and inspect their connection status.
  Host {
    #[command(subcommand)]
    command: host::Command,
  },
  /// Inspect and remove locally saved SSH passwords and key passphrases.
  Passwords {
    /// Print metadata or action results as JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<passwords::Command>,
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
  /// Manage VPNs locally or on the selected SSH host.
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
  if let Err(error) = bundled::register() {
    eprintln!("ctl: {error}");
    std::process::exit(1);
  }
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
  fn passwords_exposes_only_inspection_and_removal_with_global_json_output() {
    let arguments = Arguments::try_parse_from(["ctl", "passwords"]).unwrap();
    assert!(matches!(
      arguments.command,
      Command::Passwords {
        command: None,
        json: false
      }
    ));
    for action in ["list", "show", "remove", "clear"] {
      let mut command = vec!["ctl", "passwords", action];
      if action == "show" {
        command.push("saved-id");
      }
      command.push("--json");
      let arguments = Arguments::try_parse_from(command).unwrap();
      assert!(matches!(
        arguments.command,
        Command::Passwords { json: true, .. }
      ));
    }
    for action in ["create", "update", "import", "reveal", "copy"] {
      assert!(Arguments::try_parse_from(["ctl", "passwords", action]).is_err());
    }
    for action in ["remove", "clear"] {
      assert!(Arguments::try_parse_from(["ctl", "passwords", action, "--yes"]).is_err());
      assert!(
        Arguments::try_parse_from(["ctl", "passwords", action, "--password", "secret"]).is_err()
      );
    }
    assert!(Arguments::try_parse_from(["ctl", "passwords", "show"]).is_err());
  }

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
  fn vpn_commands_use_saved_profiles_and_expose_the_questionnaire() {
    let arguments = Arguments::try_parse_from(["ctl", "vpn", "start"]).unwrap();
    assert_eq!(arguments.host, None);
    assert!(matches!(
      arguments.command,
      Command::Vpn {
        command: vpn::Command::Start {
          profile: None,
          json: false
        }
      }
    ));
    let arguments = Arguments::try_parse_from(["ctl", "vpn", "start", "Work VPN"]).unwrap();
    assert!(matches!(
      arguments.command,
      Command::Vpn { command: vpn::Command::Start { profile: Some(profile), json: false } }
        if profile == "Work VPN"
    ));
    for action in ["create", "list", "start", "stop", "remove"] {
      assert!(Arguments::try_parse_from(["ctl", "vpn", action]).is_ok());
      assert!(Arguments::try_parse_from(["ctl", "vpn", action, "--env-file", ".env"]).is_err());
    }
    for action in ["create", "start", "list", "stop", "remove"] {
      let arguments = Arguments::try_parse_from(["ctl", "vpn", action, "--json"]).unwrap();
      assert!(matches!(
        arguments.command,
        Command::Vpn {
          command: vpn::Command::Start { json: true, .. }
            | vpn::Command::Create { json: true }
            | vpn::Command::List { json: true }
            | vpn::Command::Stop { json: true, .. }
            | vpn::Command::Remove { json: true, .. }
        }
      ));
    }
    let arguments = Arguments::try_parse_from(["ctl", "vpn", "stop", "test-vpn"]).unwrap();
    assert!(matches!(
      arguments.command,
      Command::Vpn { command: vpn::Command::Stop { profile: Some(profile), json: false } }
        if profile == "test-vpn"
    ));
    let arguments = Arguments::try_parse_from(["ctl", "vpn", "remove", "Work VPN"]).unwrap();
    assert!(matches!(
      arguments.command,
      Command::Vpn { command: vpn::Command::Remove { profile: Some(profile), json: false } }
        if profile == "Work VPN"
    ));
    assert!(Arguments::try_parse_from(["ctl", "vpn", "remove", "--yes"]).is_err());
    assert!(Arguments::try_parse_from(["ctl", "vpn", "status"]).is_err());
  }

  #[test]
  fn vpn_rejects_removed_commands_and_provider_specific_start_flags() {
    for command in ["connect", "start-tailscale"] {
      assert!(Arguments::try_parse_from(["ctl", "vpn", command]).is_err());
    }
    for flag in [
      "--env-file",
      "--id",
      "--hostname",
      "--auth-key",
      "--accept-routes",
    ] {
      assert!(Arguments::try_parse_from(["ctl", "vpn", "start", flag]).is_err());
    }
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
