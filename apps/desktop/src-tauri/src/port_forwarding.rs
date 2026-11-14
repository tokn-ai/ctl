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
  use tokio::net::{TcpListener, UdpSocket};

  async fn reserve_tcp_and_udp() -> (TcpListener, UdpSocket) {
    // A kernel-assigned TCP port may already be occupied by UDP.
    for _ in 0..16 {
      let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
      let address = tcp.local_addr().unwrap();
      match UdpSocket::bind(address).await {
        Ok(udp) => return (tcp, udp),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {}
        Err(error) => panic!("could not reserve UDP test socket: {error}"),
      }
    }
    panic!("could not reserve a port for both TCP and UDP");
  }

  #[tokio::test]
  async fn availability_checks_only_the_requested_protocols() {
    let (tcp, udp) = reserve_tcp_and_udp().await;
    let port = tcp.local_addr().unwrap().port();
    drop(udp);
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
    let (other_tcp, udp) = reserve_tcp_and_udp().await;
    let port = udp.local_addr().unwrap().port();
    drop(other_tcp);
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
    // Port zero is internal here, so each probe gets a free kernel-assigned port.
    assert!(check_bindings(0, PortForwardProtocol::Any).await.is_ok());
  }

  #[test]
  fn legacy_availability_requests_default_to_tcp() {
    let request: CheckLocalPortRequest = serde_json::from_str(r#"{"port":8080}"#).unwrap();
    assert_eq!(request.protocol, PortForwardProtocol::Tcp);
  }
}
