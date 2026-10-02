use clap::{Parser, Subcommand};
use ctl_agent::{ConnectConfig, Service, connect_stdio};
use std::env;
use std::path::PathBuf;
use thiserror::Error;
use tokio::io::AsyncReadExt as _;

#[derive(Debug, Parser)]
#[command(
  version,
  arg_required_else_help = true,
  args_conflicts_with_subcommands = true,
  about = "SSH remote-command gateway for ctl services"
)]
struct Arguments {
  /// Print version/build/protocol metadata without connecting to a service.
  #[arg(long, exclusive = true)]
  component_info: bool,
  #[command(subcommand)]
  command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Relay this SSH channel's standard streams to a fixed local service.
  Connect {
    #[arg(long, value_enum, default_value_t = Service::Ctmux)]
    service: Service,
    /// Send stable environment identity and installed version before relaying.
    #[arg(long)]
    identity: bool,
  },
  /// List TCP listeners without exposing process arguments or environment.
  Listeners,
  /// Print installed agent identity without opening or starting a service.
  Inspect,
  /// End all ctmux sessions and start the installed daemon after explicit confirmation.
  RestartCtmux,
  /// Inspect the existing ctmux owner and wait for a separate confirmation frame.
  PrepareCtmuxRestart,
}

#[tokio::main]
async fn main() {
  if let Err(error) = run(Arguments::parse()).await {
    eprintln!("ctl-agent: {error}");
    std::process::exit(1);
  }
}

async fn run(arguments: Arguments) -> Result<(), MainError> {
  if arguments.component_info {
    let info = ctl_core::component::ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: vec![ctl_core::component::ProtocolInfo {
        name: "ctl_identity".into(),
        version: ctl_proto::IDENTITY_PROTOCOL_VERSION,
      }],
    };
    println!("{}", serde_json::to_string(&info)?);
    return Ok(());
  }
  match arguments
    .command
    .expect("a subcommand is required unless component info was requested")
  {
    Command::Connect { service, identity } => {
      let mut config = ConnectConfig::new(ctmux_ipc::socket_path());
      config.service = service;
      if identity {
        config.identity = Some(
          tokio::task::spawn_blocking(ctl_agent::identity::discover)
            .await
            .map_err(std::io::Error::other)??,
        );
      }
      config.ctmuxd_bin = companion_binary("ctmuxd");
      config.taskd_bin = companion_binary("ctl-taskd");
      connect_stdio(&config).await?;
    }
    Command::PrepareCtmuxRestart => {
      let identity = tokio::task::spawn_blocking(ctl_agent::identity::inspect)
        .await
        .map_err(std::io::Error::other)??;
      let mut config = ConnectConfig::new(ctmux_ipc::socket_path());
      config.ctmuxd_bin = companion_binary("ctmuxd");
      ctl_agent::maintenance::prepare_ctmux_restart(
        &mut tokio::io::stdin(),
        &mut tokio::io::stdout(),
        &config,
        &identity.remote_id,
      )
      .await?;
    }
    Command::RestartCtmux => {
      let mut input = Vec::new();
      tokio::io::stdin()
        .take(8193)
        .read_to_end(&mut input)
        .await?;
      if input.len() > 8192 {
        return Err(std::io::Error::other("Restart request is too large.").into());
      }
      let request: ctl_proto::RemoteCtmuxRestartRequest = serde_json::from_slice(&input)?;
      let identity = tokio::task::spawn_blocking(ctl_agent::identity::discover)
        .await
        .map_err(std::io::Error::other)??;
      let mut config = ConnectConfig::new(ctmux_ipc::socket_path());
      config.ctmuxd_bin = companion_binary("ctmuxd");
      let result = ctl_agent::restart::restart_ctmux(
        &config,
        &request.expected_remote_id,
        &identity.remote_id,
      )
      .await?;
      println!("{}", serde_json::to_string(&result)?);
    }
    Command::Inspect => {
      let identity = tokio::task::spawn_blocking(ctl_agent::identity::inspect)
        .await
        .map_err(std::io::Error::other)??;
      println!("{}", serde_json::to_string(&identity)?);
    }
    Command::Listeners => {
      let catalog = tokio::task::spawn_blocking(ctl_agent::listeners::discover)
        .await
        .map_err(std::io::Error::other)?;
      println!("{}", serde_json::to_string(&catalog)?);
    }
  }
  Ok(())
}

fn companion_binary(name: &str) -> Option<PathBuf> {
  let current = env::current_exe().ok()?;
  let sibling = current.with_file_name(format!("{name}{}", env::consts::EXE_SUFFIX));
  (sibling.is_absolute() && sibling.is_file()).then_some(sibling)
}

#[derive(Debug, Error)]
enum MainError {
  #[error(transparent)]
  Agent(#[from] ctl_agent::AgentError),
  #[error("remote operation failed: {0}")]
  Identity(#[from] std::io::Error),
  #[error("invalid remote operation JSON: {0}")]
  Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn component_metadata_never_requires_a_remote_service_command() {
    let request = Arguments::try_parse_from(["ctl-agent", "--component-info"]).unwrap();
    assert!(request.component_info);
    assert!(request.command.is_none());
    assert!(Arguments::try_parse_from(["ctl-agent", "--component-info", "connect"]).is_err());
  }

  #[test]
  fn connect_defaults_to_ctmux_and_accepts_task_service() {
    assert!(matches!(
      Arguments::try_parse_from(["ctl-agent", "connect"])
        .unwrap()
        .command
        .unwrap(),
      Command::Connect {
        service: Service::Ctmux,
        identity: false
      }
    ));
    assert!(matches!(
      Arguments::try_parse_from(["ctl-agent", "connect", "--service", "task"])
        .unwrap()
        .command
        .unwrap(),
      Command::Connect {
        service: Service::Task,
        identity: false
      }
    ));
  }

  #[test]
  fn connect_accepts_identity_for_each_service() {
    for (args, expected) in [
      (vec!["ctl-agent", "connect", "--identity"], Service::Ctmux),
      (
        vec!["ctl-agent", "connect", "--identity", "--service", "task"],
        Service::Task,
      ),
    ] {
      let Command::Connect { service, identity } =
        Arguments::try_parse_from(args).unwrap().command.unwrap()
      else {
        panic!("expected connect command")
      };
      assert_eq!(service, expected);
      assert!(identity);
    }
  }

  #[test]
  fn connect_rejects_arbitrary_endpoints_and_commands() {
    for arguments in [
      vec!["connect", "--service", "control"],
      vec!["connect", "--service", "ctl-taskd"],
      vec!["connect", "--service", "/tmp/ctl-taskd.sock"],
      vec!["connect", "--socket", "/tmp/ctl-taskd.sock"],
      vec![
        "connect",
        "--service",
        "task",
        "--taskd-bin",
        "/tmp/ctl-taskd",
      ],
      vec!["connect", "--service", "task", "sh"],
      vec!["connect", "--service", "task; sh"],
      vec!["exec", "sh"],
    ] {
      assert!(
        Arguments::try_parse_from(std::iter::once("ctl-agent").chain(arguments.clone())).is_err(),
        "must reject {arguments:?}"
      );
    }
  }

  #[test]
  fn restart_is_a_fixed_operation_without_arbitrary_targets() {
    assert!(matches!(
      Arguments::try_parse_from(["ctl-agent", "restart-ctmux"])
        .unwrap()
        .command
        .unwrap(),
      Command::RestartCtmux
    ));
    for operation in ["restart-ctmux", "prepare-ctmux-restart", "inspect"] {
      assert!(Arguments::try_parse_from(["ctl-agent", operation]).is_ok());
      for argument in ["--socket", "--pid", "--command", "--service"] {
        assert!(Arguments::try_parse_from(["ctl-agent", operation, argument, "anything"]).is_err());
      }
    }
  }

  #[test]
  fn listeners_is_a_fixed_argument_free_operation() {
    assert!(matches!(
      Arguments::try_parse_from(["ctl-agent", "listeners"])
        .unwrap()
        .command
        .unwrap(),
      Command::Listeners
    ));
    assert!(Arguments::try_parse_from(["ctl-agent", "listeners", "--command", "sh"]).is_err());
  }
}
