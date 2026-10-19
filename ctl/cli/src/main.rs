mod bundled;
mod caller_auth;
mod commands;
mod components;
mod connection;
mod history;
mod host;
mod openssh;
mod passwords;
mod port;
mod remote;
mod setup;
mod skill;
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
  /// Select a saved ctl host by name or ID instead of local.
  #[arg(long, short = 'H', global = true, value_name = "HOST")]
  host: Option<String>,

  /// Connect to a temporary SSH destination or config alias, without saving a host.
  /// Supply this option before the subcommand.
  #[arg(long, id = "remote_destination", value_name = "DESTINATION", conflicts_with_all = ["host", "method"])]
  destination: Option<String>,

  /// Override the SSH user for a temporary shell or exec destination.
  #[arg(long, requires = "remote_destination")]
  user: Option<String>,

  /// Override the SSH port for a temporary shell or exec destination.
  #[arg(long, requires = "remote_destination", value_parser = clap::value_parser!(u16).range(1..))]
  port: Option<u16>,

  /// Authenticate a fresh connection using only this private key (no broker).
  #[arg(long, requires = "remote_destination", conflicts_with_all = ["password_fd", "password_pipe", "password_socket"])]
  identity_file: Option<std::path::PathBuf>,

  /// Read one UTF-8 password until EOF from inherited Unix FD N >= 3.
  #[arg(long, requires = "remote_destination", conflicts_with_all = ["password_pipe", "password_socket"], value_parser = clap::value_parser!(i32).range(3..))]
  password_fd: Option<i32>,

  /// Receive a password on an owner-only local Windows named pipe created by ctl.
  #[arg(
    long,
    requires = "remote_destination",
    conflicts_with = "password_socket"
  )]
  password_pipe: Option<std::path::PathBuf>,

  /// Receive a password on a Unix socket in a caller-owned private directory.
  #[arg(long, requires = "remote_destination")]
  password_socket: Option<std::path::PathBuf>,

  #[arg(skip)]
  password_input: Option<std::fs::File>,

  /// Select a saved host's connection method by name or ID.
  #[arg(long, global = true)]
  method: Option<String>,

  /// Remote server platform (Windows currently requires the cmd.exe SSH shell).
  #[arg(long, global = true, value_enum)]
  remote_platform: Option<RemotePlatform>,

  #[command(subcommand)]
  command: Command,
}

impl Arguments {
  fn has_remote_target(&self) -> bool {
    self.host.is_some() || self.destination.is_some()
  }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RemotePlatform {
  Unix,
  Windows,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Review local daemon diagnostics without starting ctld.
  Logs(history::Arguments),
  /// Review local connection and credential audit history.
  Audit(history::Arguments),
  /// Inspect, stop, and restart local daemons; manage component bundles on Unix.
  Components {
    #[command(subcommand)]
    command: components::Command,
  },
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
    /// Pick an existing session, creating one only when none are running.
    #[arg(long, conflicts_with_all = ["session", "plain", "cwd"])]
    resume: bool,
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

fn main() {
  // Claim caller descriptors before Tokio allocates internal descriptors. An
  // invalid inherited FD must never accidentally select a runtime-owned socket.
  let arguments = if std::env::var_os(caller_auth::ASKPASS_MODE).is_some()
    || std::env::var_os(openssh::SCP_TRANSPORT_ENV).is_some()
  {
    None
  } else {
    let mut arguments = Arguments::parse();
    if let Err(error) = caller_auth::claim(&mut arguments) {
      eprintln!("ctl: {error}");
      std::process::exit(1);
    }
    Some(arguments)
  };
  let runtime = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("could not start ctl runtime");
  let code = runtime.block_on(run_main(arguments));
  // Drop authentication tasks (and zeroize their buffers) before process exit.
  drop(runtime);
  std::process::exit(code);
}

async fn run_main(arguments: Option<Arguments>) -> i32 {
  if std::env::var_os(caller_auth::ASKPASS_MODE).is_some() {
    return match caller_auth::askpass().await {
      Ok(()) => 0,
      Err(error) => {
        eprintln!("ctl: {error}");
        1
      }
    };
  }
  if let Err(error) = bundled::register() {
    eprintln!("ctl: {error}");
    return 1;
  }
  let result = if std::env::var_os(openssh::SCP_TRANSPORT_ENV).is_some() {
    openssh::run_ssh(
      std::env::args_os().skip(1).collect(),
      std::env::var("CTL_SCP_METHOD").ok().as_deref(),
    )
    .await
    .map_err(commands::CliError::from)
  } else {
    commands::run(arguments.expect("ordinary invocations have parsed arguments")).await
  };
  match result {
    Ok(code) => code,
    Err(error) => {
      eprintln!("ctl: {error}");
      1
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn caller_credentials_require_a_temporary_native_operation_and_one_source() {
    for arguments in [
      vec!["ctl", "--identity-file", "key", "exec", "true"],
      vec![
        "ctl",
        "--destination",
        "host",
        "--port",
        "0",
        "exec",
        "true",
      ],
      vec![
        "ctl",
        "--destination",
        "host",
        "--password-fd",
        "0",
        "exec",
        "true",
      ],
      vec![
        "ctl",
        "--destination",
        "host",
        "--identity-file",
        "key",
        "--password-fd",
        "3",
        "shell",
      ],
      vec![
        "ctl",
        "--destination",
        "host",
        "--password-fd",
        "3",
        "--password-pipe",
        "pipe",
        "shell",
      ],
      vec![
        "ctl",
        "--destination",
        "host",
        "--password-socket",
        "socket",
        "--identity-file",
        "key",
        "shell",
      ],
    ] {
      assert!(Arguments::try_parse_from(arguments).is_err());
    }
    for command in ["ctmux", "port"] {
      let invalid = Arguments::try_parse_from([
        "ctl",
        "--destination",
        "host",
        "--identity-file",
        "key",
        command,
        "list",
      ])
      .unwrap();
      assert!(caller_auth::validate(&invalid).is_err());
    }
    let valid = Arguments::try_parse_from([
      "ctl",
      "--destination",
      "operator@::1",
      "--port",
      "2222",
      "--identity-file",
      "key",
      "shell",
    ])
    .unwrap();
    assert!(caller_auth::validate(&valid).is_ok());
  }

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
  fn shell_resume_uses_the_selected_host_and_rejects_creation_flags() {
    let arguments = Arguments::try_parse_from(["ctl", "-H", "work", "shell", "--resume"]).unwrap();
    assert_eq!(arguments.host.as_deref(), Some("work"));
    assert!(matches!(
      arguments.command,
      Command::Shell {
        resume: true,
        session: None,
        plain: false,
        cwd: None
      }
    ));
    for conflicting in [
      &["--session", "work"][..],
      &["--plain"][..],
      &["--cwd", "/tmp"][..],
    ] {
      let mut arguments = vec!["ctl", "shell", "--resume"];
      arguments.extend_from_slice(conflicting);
      assert!(Arguments::try_parse_from(arguments).is_err());
    }
    assert!(matches!(
      Arguments::try_parse_from(["ctl", "shell"]).unwrap().command,
      Command::Shell { resume: false, .. }
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
  fn ctmux_rename_preserves_the_explicit_host_and_target() {
    let arguments = Arguments::try_parse_from([
      "ctl",
      "-H",
      "workstation",
      "ctmux",
      "rename",
      "-t",
      "development",
      "review",
    ])
    .unwrap();
    assert_eq!(arguments.host.as_deref(), Some("workstation"));
    assert!(matches!(
      arguments.command,
      Command::Ctmux { command: CtmuxCommand::Rename { target, name } }
        if target == "development" && name == "review"
    ));
  }

  #[test]
  fn host_scoped_ctmux_resume_requires_no_additional_command() {
    let arguments = Arguments::try_parse_from(["ctl", "-H", "work", "ctmux", "--resume"]).unwrap();
    assert_eq!(arguments.host.as_deref(), Some("work"));
    assert!(matches!(
      arguments.command,
      Command::Ctmux {
        command: CtmuxCommand::Resume
      }
    ));
    for arguments in [
      &["ctl", "ctmux"][..],
      &["ctl", "ctmux", "--resume", "list"][..],
      &["ctl", "ctmux", "list", "--resume"][..],
    ] {
      assert!(Arguments::try_parse_from(arguments).is_err());
    }
  }

  #[test]
  fn remote_platform_rejects_arbitrary_commands() {
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
  fn temporary_destinations_select_remote_commands_without_saved_host_options() {
    for command in [
      &["shell", "--resume"][..],
      &["exec", "--", "true"][..],
      &["ctmux", "list"][..],
      &["task", "list"][..],
      &["port", "list"][..],
      &["vpn", "list"][..],
    ] {
      let arguments = Arguments::try_parse_from(
        [
          "ctl",
          "--destination",
          "user@work",
          "--remote-platform",
          "windows",
        ]
        .into_iter()
        .chain(command.iter().copied()),
      )
      .unwrap();
      assert_eq!(arguments.destination.as_deref(), Some("user@work"));
      assert!(arguments.host.is_none());
      assert!(arguments.has_remote_target());
    }
    for arguments in [
      vec!["ctl", "--destination", "work", "-H", "saved", "shell"],
      vec!["ctl", "--destination", "work", "--method", "vpn", "shell"],
      vec!["ctl", "ctmux", "list", "--destination", "work"],
    ] {
      assert!(Arguments::try_parse_from(arguments).is_err());
    }
  }

  #[test]
  fn host_edit_destinations_remain_distinct_from_the_connection_selector() {
    for command in [
      vec!["host", "create", "work", "ssh-alias"],
      vec!["host", "update", "work", "--destination", "ssh-alias"],
      vec!["host", "method", "add", "work", "Direct", "ssh-alias"],
      vec![
        "host",
        "method",
        "update",
        "work",
        "Direct",
        "--destination",
        "ssh-alias",
      ],
    ] {
      let arguments =
        Arguments::try_parse_from(["ctl"].into_iter().chain(command.iter().copied())).unwrap();
      assert!(arguments.destination.is_none());
      match arguments.command {
        Command::Host {
          command: host::Command::Create(_),
        } => {}
        Command::Host {
          command:
            host::Command::Update { destination, .. }
            | host::Command::Method {
              command: host::MethodCommand::Update { destination, .. },
            },
        } => {
          assert_eq!(destination.as_deref(), Some("ssh-alias"));
        }
        Command::Host {
          command:
            host::Command::Method {
              command: host::MethodCommand::Add { destination, .. },
            },
        } => assert_eq!(destination, "ssh-alias"),
        _ => panic!("expected host edit command"),
      }
    }
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
