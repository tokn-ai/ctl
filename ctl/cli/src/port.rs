use ctl_client::hosts::ConnectionTargetDto;
use ctl_ipc::{
  ClientMessage, LocalPortForward, PortForwardState, PortForwardStatus, ServerMessage,
};

#[derive(Debug, clap::Subcommand)]
pub enum Command {
  /// Start a local forward: `[bind_address:]local_port:remote_host:remote_port`.
  Add {
    specification: String,
    /// Stable ID for updating/removing this forward.
    #[arg(long)]
    id: Option<String>,
    #[arg(long)]
    json: bool,
  },
  /// List this connection method's forwards without opening SSH.
  List {
    #[arg(long)]
    json: bool,
  },
  /// Stop and forget a forward by ID.
  Remove { id: String },
}

pub async fn run(target: &ConnectionTargetDto, command: Command) -> Result<(), Error> {
  #[cfg(not(unix))]
  if matches!(
    target,
    ConnectionTargetDto::Ssh {
      use_ssh_config_master: Some(true),
      ..
    }
  ) {
    return Err(Error::MasterSelectionUnsupported);
  }
  let ssh_target = target.to_ssh_target()?;
  match command {
    Command::Add {
      specification,
      id,
      json,
    } => {
      let implicit_id = id.is_none();
      let mut forward = parse_forward(
        &specification,
        id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
      )?;
      if implicit_id {
        let statuses = list(ssh_target.clone()).await?;
        if let Some(existing) = statuses
          .iter()
          .filter(|status| {
            let existing = &status.forward;
            existing.bind_address == forward.bind_address
              && existing.local_port == forward.local_port
              && existing.remote_host == forward.remote_host
              && existing.remote_port == forward.remote_port
          })
          .min_by(|left, right| {
            (left.state != PortForwardState::Active)
              .cmp(&(right.state != PortForwardState::Active))
              .then_with(|| left.forward.forward_id.cmp(&right.forward.forward_id))
          })
        {
          forward.forward_id.clone_from(&existing.forward.forward_id);
        }
      }
      crate::target::ensure_vpn(target).await?;
      #[cfg(unix)]
      crate::ssh_broker::ensure_master(ssh_target.clone()).await?;
      // Windows forwards own a dedicated SSH process. The helper validates and
      // establishes that transport without a multiplexed master or prompts.
      let ServerMessage::PortForwardConfigured { status } =
        crate::ssh_broker::request(ClientMessage::ConfigurePortForward {
          target: ssh_target,
          forward,
          enabled: true,
        })
        .await?
      else {
        return Err(Error::UnexpectedResponse);
      };
      if status.state != PortForwardState::Active {
        return Err(Error::Forward(status.message.unwrap_or_else(|| {
          "Port forward is waiting for authentication.".into()
        })));
      }
      print_statuses(&[status], json)?;
    }
    Command::List { json } => print_statuses(&list(ssh_target).await?, json)?,
    Command::Remove { id } => {
      let status = list(ssh_target.clone())
        .await?
        .into_iter()
        .find(|status| status.forward.forward_id == id)
        .ok_or_else(|| {
          Error::Forward(format!(
            "No forward {id:?} exists for this connection method."
          ))
        })?;
      let response = crate::ssh_broker::request(ClientMessage::ConfigurePortForward {
        target: ssh_target,
        forward: status.forward,
        enabled: false,
      })
      .await?;
      if !matches!(response, ServerMessage::PortForwardConfigured { .. }) {
        return Err(Error::UnexpectedResponse);
      }
    }
  }
  Ok(())
}

async fn list(target: ctl_ipc::SshTarget) -> Result<Vec<PortForwardStatus>, Error> {
  match crate::ssh_broker::request(ClientMessage::ListPortForwards { target }).await? {
    ServerMessage::PortForwards { statuses } => Ok(statuses),
    _ => Err(Error::UnexpectedResponse),
  }
}

fn print_statuses(statuses: &[PortForwardStatus], json: bool) -> Result<(), Error> {
  if json {
    println!("{}", serde_json::to_string(statuses)?);
  } else {
    let rows = statuses.iter().map(|status| {
      let forward = &status.forward;
      [
        forward.forward_id.clone(),
        endpoint(&forward.bind_address, forward.local_port),
        endpoint(&forward.remote_host, forward.remote_port),
        format!("{:?}", status.state),
      ]
    });
    println!(
      "{}",
      crate::table::format(["ID", "LOCAL", "REMOTE", "STATUS"], rows)
    );
    for status in statuses {
      if let Some(message) = &status.message {
        println!(
          "{}: {}",
          crate::table::text(&status.forward.forward_id),
          crate::table::text(message)
        );
      }
    }
  }
  Ok(())
}

fn endpoint(host: &str, port: u16) -> String {
  if host.contains(':') {
    format!("[{host}]:{port}")
  } else {
    format!("{host}:{port}")
  }
}

fn parse_forward(value: &str, forward_id: String) -> Result<LocalPortForward, Error> {
  let mut brackets = false;
  let parts: Vec<_> = value
    .split(|character| match character {
      '[' => {
        brackets = true;
        false
      }
      ']' => {
        brackets = false;
        false
      }
      ':' => !brackets,
      _ => false,
    })
    .collect();
  let (bind, local, remote, port) = match parts.as_slice() {
    [local, remote, port] => ("127.0.0.1", *local, *remote, *port),
    [bind, local, remote, port] => (*bind, *local, *remote, *port),
    _ => return Err(Error::InvalidSpecification),
  };
  let bind = bind.trim_matches(['[', ']']);
  let remote = remote.trim_matches(['[', ']']);
  let port_number = |value: &str| {
    value
      .parse::<u16>()
      .ok()
      .filter(|port| *port > 0)
      .ok_or(Error::InvalidSpecification)
  };
  if !matches!(bind, "127.0.0.1" | "::1")
    || remote.is_empty()
    || remote
      .chars()
      .any(|character| character.is_whitespace() || character.is_control())
  {
    return Err(Error::InvalidSpecification);
  }
  Ok(LocalPortForward {
    forward_id,
    bind_address: bind.into(),
    local_port: port_number(local)?,
    remote_host: remote.into(),
    remote_port: port_number(port)?,
  })
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[cfg(not(unix))]
  #[error("SSH master selection currently requires macOS or Linux.")]
  MasterSelectionUnsupported,
  #[error(transparent)]
  Host(#[from] ctl_client::hosts::HostError),
  #[error(transparent)]
  Target(#[from] crate::target::Error),
  #[error(transparent)]
  Broker(#[from] crate::ssh_broker::Error),
  #[error(transparent)]
  Json(#[from] serde_json::Error),
  #[error("ctld returned an unexpected port-forward response")]
  UnexpectedResponse,
  #[error("{0}")]
  Forward(String),
  #[error(
    "Use [127.0.0.1: or [::1]:]local_port:remote_host:remote_port, with ports between 1 and 65535."
  )]
  InvalidSpecification,
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn forwarding_supports_ipv6_without_binding_public_interfaces() {
    let forward = parse_forward("[::1]:8080:[2001:db8::2]:80", "web".into()).unwrap();
    assert_eq!(forward.bind_address, "::1");
    assert_eq!(forward.remote_host, "2001:db8::2");
    for value in [
      "0.0.0.0:8080:localhost:80",
      "0:localhost:80",
      "8080:host:65536",
      "8080::80",
    ] {
      assert!(parse_forward(value, "web".into()).is_err());
    }
  }

  #[cfg(not(unix))]
  #[tokio::test]
  async fn explicit_master_selection_is_rejected_before_helper_startup() {
    let target = serde_json::from_value(serde_json::json!({
      "kind": "ssh", "destination": "configured", "ssh_config_alias": "configured",
      "use_ssh_config_master": true,
    }))
    .unwrap();
    assert!(matches!(
      run(&target, Command::List { json: true }).await,
      Err(Error::MasterSelectionUnsupported),
    ));
  }
}
