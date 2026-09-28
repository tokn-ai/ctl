use super::*;
use crate::{VpnConnection, VpnState};
use zeroize::Zeroizing;

fn connection() -> VpnConnection {
  VpnConnection {
    connection_id: "test".into(),
    name: "Test VPN".into(),
    url: "https://vpn.example.test".into(),
    username: "test-user".into(),
    password: Zeroizing::new("literal $password='value'\\tail".into()),
    auth_method: None,
    target_ip: None,
  }
}

#[test]
fn connection_validation_rejects_env_injection_without_echoing_values() {
  connection().validate().unwrap();
  let mut spaces = connection();
  spaces.password = Zeroizing::new("   ".into());
  spaces.validate().unwrap();
  for forbidden in ["\n", "\r", "\0"] {
    for field in 0..7 {
      let mut candidate = connection();
      let value = format!("private-marker{forbidden}VPN_PASSWORD=injected");
      match field {
        0 => candidate.connection_id = value,
        1 => candidate.name = value,
        2 => candidate.url = value,
        3 => candidate.username = value,
        4 => candidate.password = Zeroizing::new(value),
        5 => candidate.auth_method = Some(value),
        _ => candidate.target_ip = Some(value),
      }
      let message = candidate.validate().unwrap_err();
      assert!(!message.contains("private-marker"));
      assert!(!message.contains("injected"));
    }
  }
  for url in [
    "http://vpn.example.test",
    "https://",
    "https://user@vpn.example.test",
  ] {
    let mut candidate = connection();
    candidate.url = url.into();
    assert!(candidate.validate().is_err());
  }
  let mut candidate = connection();
  candidate.target_ip = Some("192.0.2.25".into());
  candidate.validate().unwrap();
  candidate.target_ip = Some("not-an-ip".into());
  assert!(candidate.validate().is_err());
}

#[tokio::test]
async fn structured_connection_round_trips_and_preserves_lifecycle_state() {
  let (mut client, mut server) = tokio::io::duplex(16384);
  let expected = VpnStatus {
    state: VpnState::Connected,
    connection_id: Some("test".into()),
    running: true,
    endpoint: Some("socks5h://127.0.0.1:49152".into()),
    container_name: Some("test-container".into()),
    vpn_url: Some("https://vpn.example.test".into()),
    username: Some("test-user".into()),
  };
  let response = expected.clone();
  let daemon = tokio::spawn(async move {
    assert!(matches!(
      crate::read_frame(&mut server).await.unwrap(),
      Some(ClientMessage::Handshake { .. })
    ));
    crate::write_frame(
      &mut server,
      &ServerMessage::HandshakeAccepted {
        protocol_version: crate::PROTOCOL_VERSION,
      },
    )
    .await
    .unwrap();
    let Some(ClientMessage::StartVpnConnection { connection: actual }) =
      crate::read_frame(&mut server).await.unwrap()
    else {
      panic!("expected saved connection");
    };
    assert!(actual == connection());
    crate::write_frame(&mut server, &ServerMessage::VpnStatus { status: response })
      .await
      .unwrap();
  });
  let actual = exchange(
    &mut client,
    &ClientMessage::StartVpnConnection {
      connection: connection(),
    },
  )
  .await
  .unwrap();
  assert_eq!(actual, expected);
  assert_eq!(serde_json::to_value(actual).unwrap()["state"], "connected");
  daemon.await.unwrap();
}

#[tokio::test]
async fn handshake_rejects_mismatched_protocol_and_preserves_daemon_error_codes() {
  for response in [
    ServerMessage::HandshakeAccepted {
      protocol_version: crate::PROTOCOL_VERSION - 1,
    },
    ServerMessage::Error {
      code: "synthetic_failure".into(),
      message: "synthetic diagnostic".into(),
    },
  ] {
    let (mut client, mut server) = tokio::io::duplex(1024);
    let daemon = tokio::spawn(async move {
      let _: Option<ClientMessage> = crate::read_frame(&mut server).await.unwrap();
      crate::write_frame(&mut server, &response).await.unwrap();
    });
    let error = exchange(&mut client, &ClientMessage::VpnStatus)
      .await
      .unwrap_err();
    assert!(matches!(
      error.code(),
      "ctld_protocol_version_mismatch" | "synthetic_failure"
    ));
    daemon.await.unwrap();
  }
}

#[test]
fn status_from_older_protocol_eleven_daemon_defaults_missing_metadata() {
  let status: VpnStatus = serde_json::from_str(
    r#"{
    "state":"connected","connection_id":"test","running":true,
    "endpoint":"socks5h://127.0.0.1:49152","container_name":"test-container"
  }"#,
  )
  .unwrap();
  assert!(status.running);
  assert_eq!(status.vpn_url, None);
  assert_eq!(status.username, None);
  assert_eq!(crate::PROTOCOL_VERSION, 11);
}
