use ctl_ipc::PortForwardProtocol;
use serde::{Deserialize, Serialize};

use crate::error::{CommandErrorDto, CommandResult};

#[derive(Deserialize)]
pub struct CheckLocalPortRequest {
  port: u16,
  #[serde(default)]
  protocol: PortForwardProtocol,
}

#[derive(Serialize)]
pub struct LocalPortAvailability {
  port: u16,
  available: bool,
  message: Option<String>,
}

#[tauri::command]
pub async fn check_local_port(
  request: CheckLocalPortRequest,
) -> CommandResult<LocalPortAvailability> {
  if request.port == 0 {
    return Err(CommandErrorDto::new(
      "invalid_local_port",
      "Local ports must be between 1 and 65535.",
    ));
  }
  match check_bindings(request.port, request.protocol).await {
    Ok(()) => Ok(LocalPortAvailability {
      port: request.port,
      available: true,
      message: None,
    }),
    Err(error) => Ok(LocalPortAvailability {
      port: request.port,
      available: false,
      message: Some(error.to_string()),
    }),
  }
}

async fn check_bindings(port: u16, protocol: PortForwardProtocol) -> std::io::Result<()> {
  let tcp = if protocol.includes_tcp() {
    Some(tokio::net::TcpListener::bind(("127.0.0.1", port)).await?)
  } else {
    None
  };
  let udp = if protocol.includes_udp() {
    Some(tokio::net::UdpSocket::bind(("127.0.0.1", port)).await?)
  } else {
    None
  };
  // Keep both bindings reserved until the complete Any preflight succeeds.
  drop((tcp, udp));
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn availability_checks_only_the_requested_protocols() {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    assert!(
      check_bindings(port, PortForwardProtocol::Tcp)
        .await
        .is_err()
    );
    assert!(
      check_bindings(port, PortForwardProtocol::Any)
        .await
        .is_err()
    );
    assert!(check_bindings(port, PortForwardProtocol::Udp).await.is_ok());
    drop(tcp);

    let udp = tokio::net::UdpSocket::bind(("127.0.0.1", port))
      .await
      .unwrap();
    assert!(check_bindings(port, PortForwardProtocol::Tcp).await.is_ok());
    assert!(
      check_bindings(port, PortForwardProtocol::Udp)
        .await
        .is_err()
    );
    assert!(
      check_bindings(port, PortForwardProtocol::Any)
        .await
        .is_err()
    );
    drop(udp);
    assert!(check_bindings(port, PortForwardProtocol::Any).await.is_ok());
  }

  #[test]
  fn legacy_availability_requests_default_to_tcp() {
    let request: CheckLocalPortRequest = serde_json::from_str(r#"{"port":8080}"#).unwrap();
    assert_eq!(request.protocol, PortForwardProtocol::Tcp);
  }
}
