use clap::{Parser, Subcommand};
use ctl_agent::{ConnectConfig, Service, connect_stdio};
use std::env;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(version, about = "SSH remote-command gateway for ctl services")]
struct Arguments {
  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// Relay this SSH channel's standard streams to a fixed local service.
  Connect {
    #[arg(long, value_enum, default_value_t = Service::Rmux)]
    service: Service,
  },
}

#[tokio::main]
async fn main() {
  if let Err(error) = run(Arguments::parse()).await {
    eprintln!("ctl-agent: {error}");
    std::process::exit(1);
  }
}

async fn run(arguments: Arguments) -> Result<(), MainError> {
  match arguments.command {
    Command::Connect { service } => {
      let mut config = ConnectConfig::new(rmux_ipc::socket_path());
      config.service = service;
      config.rmuxd_bin = companion_binary("rmuxd");
      config.taskd_bin = companion_binary("taskd");
      connect_stdio(&config).await?;
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
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn connect_defaults_to_rmux_and_accepts_task_service() {
    assert!(matches!(
      Arguments::try_parse_from(["ctl-agent", "connect"])
        .unwrap()
        .command,
      Command::Connect {
        service: Service::Rmux
      }
    ));
    assert!(matches!(
      Arguments::try_parse_from(["ctl-agent", "connect", "--service", "task"])
        .unwrap()
        .command,
      Command::Connect {
        service: Service::Task
      }
    ));
  }

  #[test]
  fn connect_rejects_arbitrary_endpoints_and_commands() {
    for arguments in [
      vec!["connect", "--service", "control"],
      vec!["connect", "--service", "taskd"],
      vec!["connect", "--service", "/tmp/taskd.sock"],
      vec!["connect", "--socket", "/tmp/taskd.sock"],
      vec!["connect", "--service", "task", "--taskd-bin", "/tmp/taskd"],
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
}
