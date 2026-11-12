use ctl_client::{RemotePlatform, hosts::ConnectionTargetDto};
use ctl_ipc::{
  ClientMessage, LocalPortForward, PortForwardProtocol, PortForwardState, PortForwardStatus,
  ServerMessage,
};

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum Protocol {
  #[default]
  Tcp,
  Udp,
  Any,
}

impl From<Protocol> for PortForwardProtocol {
  fn from(protocol: Protocol) -> Self {
    match protocol {
      Protocol::Tcp => Self::Tcp,
      Protocol::Udp => Self::Udp,
      Protocol::Any => Self::Any,
    }
  }
}

#[derive(Debug, clap::Subcommand)]
pub enum Command {
  /// Start a local forward: `[bind_address:]local_port:remote_host:remote_port`.
  Add {
    specification: String,
    /// Forward TCP, UDP, or both protocols on the same local port.
    #[arg(long, value_enum, default_value_t = Protocol::Tcp)]
    protocol: Protocol,
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
  if target.remote_platform() == RemotePlatform::Windows
    && matches!(
      command,
      Command::Add {
        protocol: Protocol::Udp | Protocol::Any,
        ..
      }
    )
  {
    return Err(Error::WindowsUdpUnsupported);
  }
  #[cfg(not(unix))]
  if target.remote_platform() != RemotePlatform::Windows
    && matches!(
      target,
      ConnectionTargetDto::Ssh {
        use_ssh_config_master: Some(true),
        ..
      }
    )
  {
    return Err(Error::MasterSelectionUnsupported);
  }
  let ssh_target = target.to_ssh_target()?;
  match command {
    Command::Add {
      specification,
      protocol,
      id,
      json,
    } => {
      let implicit_id = id.is_none();
      let mut forward = parse_forward(
        &specification,
        id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
      )?;
      forward.protocol = protocol.into();
      if implicit_id {
        let statuses = list(ssh_target.clone()).await?;
        if let Some(existing) = statuses
          .iter()
          .filter(|status| same_forward_destination(&status.forward, &forward))
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
        match forward.protocol {
          PortForwardProtocol::Tcp => "TCP".into(),
          PortForwardProtocol::Udp => "UDP".into(),
          PortForwardProtocol::Any => "ANY".into(),
        },
        endpoint(&forward.bind_address, forward.local_port),
        endpoint(&forward.remote_host, forward.remote_port),
        format!("{:?}", status.state),
      ]
    });
    println!(
      "{}",
      crate::table::format(["ID", "PROTOCOL", "LOCAL", "REMOTE", "STATUS"], rows)
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

fn same_forward_destination(left: &LocalPortForward, right: &LocalPortForward) -> bool {
  left.protocol == right.protocol
    && left.bind_address == right.bind_address
    && left.local_port == right.local_port
    && left.remote_host == right.remote_host
    && left.remote_port == right.remote_port
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
    protocol: PortForwardProtocol::Tcp,
    bind_address: bind.into(),
    local_port: port_number(local)?,
    remote_host: remote.into(),
    remote_port: port_number(port)?,
  })
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("UDP forwarding to Windows SSH hosts is unavailable. Choose TCP forwarding.")]
  WindowsUdpUnsupported,
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
  fn implicit_forward_ids_are_reused_only_for_the_same_protocol_and_destination() {
    let tcp = parse_forward("5353:localhost:5353", "tcp".into()).unwrap();
    assert_eq!(tcp.protocol, PortForwardProtocol::Tcp);
    let mut udp = tcp.clone();
    udp.protocol = PortForwardProtocol::Udp;
    assert!(!same_forward_destination(&tcp, &udp));
    let mut both = tcp.clone();
    both.protocol = PortForwardProtocol::Any;
    assert!(!same_forward_destination(&tcp, &both));
    let mut same = tcp.clone();
    same.forward_id = "other-owner".into();
    assert!(same_forward_destination(&tcp, &same));
  }

  #[test]
  fn protocol_selection_accepts_only_tcp_udp_and_any() {
    use clap::Parser as _;

    for (value, expected) in [
      ("tcp", PortForwardProtocol::Tcp),
      ("udp", PortForwardProtocol::Udp),
      ("any", PortForwardProtocol::Any),
    ] {
      let arguments = crate::Arguments::try_parse_from([
        "ctl",
        "--destination",
        "remote.example",
        "port",
        "add",
        "5353:localhost:5353",
        "--protocol",
        value,
      ])
      .unwrap();
      let crate::Command::Port {
        command: Command::Add { protocol, .. },
      } = arguments.command
      else {
        panic!("expected port add");
      };
      assert_eq!(PortForwardProtocol::from(protocol), expected);
    }
    assert!(
      crate::Arguments::try_parse_from([
        "ctl",
        "--destination",
        "remote.example",
        "port",
        "add",
        "5353:localhost:5353",
        "--protocol",
        "sctp",
      ])
      .is_err()
    );
  }
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

  #[tokio::test]
  async fn udp_forwarding_rejects_windows_ssh_targets_before_helper_startup() {
    let target = serde_json::from_value(serde_json::json!({
      "kind": "ssh", "destination": "configured", "remote_platform": "windows",
    }))
    .unwrap();
    for protocol in [Protocol::Udp, Protocol::Any] {
      let command = Command::Add {
        specification: "5353:localhost:5353".into(),
        protocol,
        id: None,
        json: true,
      };
      assert!(matches!(
        run(&target, command).await,
        Err(Error::WindowsUdpUnsupported),
      ));
    }
  }
}
