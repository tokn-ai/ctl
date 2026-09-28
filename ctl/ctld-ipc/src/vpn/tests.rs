use super::*;
use crate::{VpnConnection, VpnSettings, VpnState};
use zeroize::Zeroizing;

#[test]
fn authentication_links_are_restricted_to_tailscale_login() {
  assert!(is_tailscale_auth_url(
    "https://login.tailscale.com/a/123abc"
  ));
  for value in [
    "http://login.tailscale.com/a/123",
    "https://login.tailscale.com.evil.test/a/123",
    "https://user@login.tailscale.com/a/123",
    "https://login.tailscale.com:8443/a/123",
    "https://login.tailscale.com/a/123?next=other",
    "https://login.tailscale.com/a/123#other",
    "https://login.tailscale.com/a/",
    "https://login.tailscale.com/a/123/other",
    "https://login.tailscale.com/a/%2f",
    "https://login.tailscale.com/a/123\n",
    "file:///a/123",
    "javascript:alert(1)",
  ] {
    assert!(!is_tailscale_auth_url(value), "{value}");
  }
}

#[test]
fn old_multi_vpn_daemons_do_not_claim_tailscale_support() {
  let old: VpnSnapshot = serde_json::from_value(serde_json::json!({
    "connections":[], "supports_multiple":true
  }))
  .unwrap();
  assert_eq!(old.supported_providers, vec![VpnProvider::Openconnect]);
  assert!(
    VpnSnapshot::default()
      .supported_providers
      .contains(&VpnProvider::Tailscale)
  );
}

fn connection() -> VpnConnection {
  VpnConnection {
    connection_id: "test".into(),
    name: "Test VPN".into(),
    settings: VpnSettings::Openconnect {
      url: "https://vpn.example.test".into(),
      username: "test-user".into(),
      password: Zeroizing::new("literal $password='value'\\tail".into()),
      auth_method: None,
      target_ip: None,
    },
  }
}

#[test]
fn connection_validation_rejects_env_injection_without_echoing_values() {
  connection().validate().unwrap();
  let mut spaces = connection();
  if let VpnSettings::Openconnect { password, .. } = &mut spaces.settings {
    *password = Zeroizing::new("   ".into());
  }
  spaces.validate().unwrap();
  for forbidden in ["\n", "\r", "\0"] {
    for field in 0..7 {
      let mut candidate = connection();
      let value = format!("private-marker{forbidden}VPN_PASSWORD=injected");
      let VpnSettings::Openconnect {
        url,
        username,
        password,
        auth_method,
        target_ip,
      } = &mut candidate.settings
      else {
        unreachable!()
      };
      match field {
        0 => candidate.connection_id = value,
        1 => candidate.name = value,
        2 => *url = value,
        3 => *username = value,
        4 => *password = Zeroizing::new(value),
        5 => *auth_method = Some(value),
        _ => *target_ip = Some(value),
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
    if let VpnSettings::Openconnect { url: address, .. } = &mut candidate.settings {
      *address = url.into();
    }
    assert!(candidate.validate().is_err());
  }
  let mut candidate = connection();
  if let VpnSettings::Openconnect { target_ip, .. } = &mut candidate.settings {
    *target_ip = Some("192.0.2.25".into());
  }
  candidate.validate().unwrap();
  if let VpnSettings::Openconnect { target_ip, .. } = &mut candidate.settings {
    *target_ip = Some("not-an-ip".into());
  }
  assert!(candidate.validate().is_err());
}

#[tokio::test]
async fn structured_connection_round_trips_and_preserves_lifecycle_state() {
  let (mut client, mut server) = tokio::io::duplex(16384);
  let expected = VpnStatus {
    vpn_id: Some("test".into()),
    state: VpnState::Connected,
    connection_id: Some("test".into()),
    running: true,
    endpoint: Some("socks5h://127.0.0.1:49152".into()),
    container_name: Some("test-container".into()),
    vpn_url: Some("https://vpn.example.test".into()),
    username: Some("test-user".into()),
    ..VpnStatus::default()
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
    crate::write_frame(
      &mut server,
      &ServerMessage::VpnStatus {
        status: response,
        snapshot: None,
      },
    )
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
  assert_eq!(actual.status, expected);
  assert_eq!(
    serde_json::to_value(actual.status).unwrap()["state"],
    "connected"
  );
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

#[cfg(unix)]
mod endpoints {
  use std::os::unix::fs::PermissionsExt as _;
  use std::sync::atomic::{AtomicU64, Ordering};

  use tokio::net::UnixListener;

  use super::*;

  struct Fixture(PathBuf);

  impl Fixture {
    fn new() -> Self {
      static NEXT: AtomicU64 = AtomicU64::new(0);
      // Keep Unix socket paths short even when macOS uses a long TMPDIR.
      let directory = PathBuf::from("/tmp").join(format!(
        "ctld-vpn-client-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
      ));
      std::fs::create_dir(&directory).unwrap();
      std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
      Self(directory)
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  async fn read_request(listener: &UnixListener) -> (crate::Stream, ClientMessage) {
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(matches!(
      crate::read_frame(&mut stream).await.unwrap(),
      Some(ClientMessage::Handshake { protocol_version }) if protocol_version == crate::PROTOCOL_VERSION
    ));
    crate::write_frame(
      &mut stream,
      &ServerMessage::HandshakeAccepted {
        protocol_version: crate::PROTOCOL_VERSION,
      },
    )
    .await
    .unwrap();
    let request = crate::read_frame(&mut stream).await.unwrap().unwrap();
    (stream, request)
  }

  #[tokio::test]
  async fn tailscale_requires_explicit_support_before_sending_settings() {
    for supported in [false, true] {
      let fixture = Fixture::new();
      let path = fixture.0.join("tailscale.sock");
      let listener = UnixListener::bind(&path).unwrap();
      let client = Client::new(path);
      let daemon = tokio::spawn(async move {
        let (mut stream, message) = read_request(&listener).await;
        assert!(matches!(message, ClientMessage::VpnStatus));
        crate::write_frame(
          &mut stream,
          &ServerMessage::VpnStatus {
            status: VpnStatus::default(),
            snapshot: Some(VpnSnapshot {
              supported_providers: if supported {
                vec![VpnProvider::Openconnect, VpnProvider::Tailscale]
              } else {
                vec![VpnProvider::Openconnect]
              },
              ..VpnSnapshot::default()
            }),
          },
        )
        .await
        .unwrap();
        drop(stream);
        if supported {
          let (mut stream, message) = read_request(&listener).await;
          assert!(
            matches!(message, ClientMessage::StartVpnConnection { connection } if connection.provider() == VpnProvider::Tailscale)
          );
          crate::write_frame(
            &mut stream,
            &ServerMessage::VpnStatus {
              status: VpnStatus {
                provider: VpnProvider::Tailscale,
                state: VpnState::Starting,
                auth_url: Some("https://login.tailscale.com/a/123abc".into()),
                ..VpnStatus::default()
              },
              snapshot: None,
            },
          )
          .await
          .unwrap();
        } else {
          assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
              .await
              .is_err()
          );
        }
      });
      let result = client
        .start_connection(VpnConnection {
          connection_id: "tailnet".into(),
          name: "Tailnet".into(),
          settings: VpnSettings::Tailscale {
            hostname: None,
            accept_routes: false,
          },
        })
        .await;
      if supported {
        let status = result.unwrap();
        assert_eq!(status.state, VpnState::Starting);
        assert!(status.auth_url.is_some());
      } else {
        assert!(matches!(result, Err(VpnError::TailscaleUnsupported)));
      }
      daemon.await.unwrap();
    }
  }

  #[tokio::test]
  async fn every_operation_uses_the_explicit_endpoint() {
    let fixture = Fixture::new();
    let selected = fixture.0.join("selected.sock");
    let other = UnixListener::bind(fixture.0.join("other.sock")).unwrap();
    let listener = UnixListener::bind(&selected).unwrap();
    let client = Client::new(selected);
    let expected = VpnStatus {
      vpn_id: Some("legacy".into()),
      endpoint: Some("socks5h://127.0.0.1:49152".into()),
      running: true,
      state: VpnState::Connected,
      ..VpnStatus::default()
    };
    let response = expected.clone();
    let daemon = tokio::spawn(async move {
      for operation in 0..4 {
        let (mut stream, request) = read_request(&listener).await;
        match (operation, request) {
          (0, ClientMessage::VpnStatus) | (3, ClientMessage::StopVpn) => {}
          (1, ClientMessage::StartVpn { env_file }) => {
            assert_eq!(env_file, std::path::absolute("test.env").unwrap());
          }
          (2, ClientMessage::StartVpnConnection { connection: actual }) => {
            assert!(actual == connection());
          }
          _ => panic!("VPN client sent an unexpected operation"),
        }
        crate::write_frame(
          &mut stream,
          &ServerMessage::VpnStatus {
            status: response.clone(),
            snapshot: None,
          },
        )
        .await
        .unwrap();
      }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
      assert_eq!(client.status().await.unwrap(), expected);
      assert_eq!(client.start("test.env".into()).await.unwrap(), expected);
      assert_eq!(
        client.start_connection(connection()).await.unwrap(),
        expected
      );
      assert_eq!(client.stop().await.unwrap(), expected);
      daemon.await.unwrap();
    })
    .await
    .unwrap();
    assert!(
      tokio::time::timeout(Duration::from_millis(20), other.accept())
        .await
        .is_err()
    );
  }

  #[tokio::test]
  async fn missing_or_stale_explicit_endpoint_does_not_start_a_daemon() {
    // A concurrently forked child can retain this listener until exec even
    // after the parent closes it, making a supposedly stale endpoint connect.
    let _execution_guard = crate::tests::SUBPROCESS_FIXTURE_LOCK.lock().await;
    let fixture = Fixture::new();
    for stale in [false, true] {
      let selected = fixture
        .0
        .join(if stale { "stale.sock" } else { "missing.sock" });
      if stale {
        drop(std::os::unix::net::UnixListener::bind(&selected).unwrap());
        assert_eq!(
          std::os::unix::net::UnixStream::connect(&selected)
            .unwrap_err()
            .kind(),
          std::io::ErrorKind::ConnectionRefused
        );
      }
      let existed = selected.exists();
      let client = Client::new(selected.clone());
      assert_eq!(client.status().await.unwrap(), VpnStatus::default());
      assert_eq!(client.stop().await.unwrap(), VpnStatus::default());
      assert_eq!(client.list().await.unwrap(), VpnSnapshot::default());
      assert_eq!(
        client.stop_id("missing").await.unwrap(),
        VpnStatus::default()
      );
      assert_eq!(selected.exists(), existed);
    }
  }

  #[tokio::test]
  async fn explicit_endpoint_connection_errors_are_not_reported_as_stopped() {
    let fixture = Fixture::new();
    let selected = fixture.0.join("not-a-directory");
    std::fs::write(&selected, "fixture").unwrap();
    let client = Client::new(selected.join("ctld.sock"));
    assert_eq!(
      client.status().await.unwrap_err().code(),
      "ctld_connection_failed"
    );
    assert_eq!(
      client.stop().await.unwrap_err().code(),
      "ctld_connection_failed"
    );
  }

  #[tokio::test]
  async fn targeted_stop_uses_the_selected_id_when_supported() {
    let fixture = Fixture::new();
    let selected = fixture.0.join("selected.sock");
    let listener = UnixListener::bind(&selected).unwrap();
    let client = Client::new(selected);
    let server = tokio::spawn(async move {
      let (mut stream, request) = read_request(&listener).await;
      assert!(matches!(request, ClientMessage::VpnStatus));
      let status = VpnStatus {
        vpn_id: Some("selected-profile".into()),
        connection_id: Some("selected-profile".into()),
        state: VpnState::Connected,
        running: true,
        ..VpnStatus::default()
      };
      let snapshot = Some(VpnSnapshot {
        connections: vec![
          status.clone(),
          VpnStatus {
            vpn_id: Some("other-profile".into()),
            state: VpnState::Starting,
            ..VpnStatus::default()
          },
        ],
        supports_multiple: true,
        ..VpnSnapshot::default()
      });
      crate::write_frame(&mut stream, &ServerMessage::VpnStatus { status, snapshot })
        .await
        .unwrap();
      let (mut stream, request) = read_request(&listener).await;
      assert!(
        matches!(request, ClientMessage::StopVpnById { vpn_id } if vpn_id == "selected-profile")
      );
      crate::write_frame(
        &mut stream,
        &ServerMessage::VpnStatus {
          status: VpnStatus::default(),
          snapshot: None,
        },
      )
      .await
      .unwrap();
    });
    assert_eq!(
      client.stop_id("selected-profile").await.unwrap(),
      VpnStatus::default()
    );
    server.await.unwrap();
  }

  #[tokio::test]
  async fn legacy_targeted_stop_never_sends_an_unqualified_stop() {
    for (connection_id, expected_error) in [
      (
        Some("selected-profile"),
        Some("vpn_targeted_stop_unsupported"),
      ),
      (Some("different-profile"), Some("vpn_not_found")),
      (None, None),
    ] {
      let fixture = Fixture::new();
      let selected = fixture.0.join("selected.sock");
      let listener = UnixListener::bind(&selected).unwrap();
      let client = Client::new(selected);
      let server = tokio::spawn(async move {
        let (mut stream, request) = read_request(&listener).await;
        assert!(matches!(request, ClientMessage::VpnStatus));
        let status = connection_id.map_or_else(VpnStatus::default, |id| VpnStatus {
          connection_id: Some(id.into()),
          state: VpnState::Connected,
          running: true,
          ..VpnStatus::default()
        });
        crate::write_frame(
          &mut stream,
          &ServerMessage::VpnStatus {
            status,
            snapshot: None,
          },
        )
        .await
        .unwrap();
        assert!(
          tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
        );
      });
      let result = client.stop_id("selected-profile").await;
      if let Some(code) = expected_error {
        let error = result.unwrap_err();
        assert_eq!(error.code(), code);
        if code == "vpn_targeted_stop_unsupported" {
          assert!(error.to_string().contains("update ctld"));
          assert!(error.to_string().contains("ctl vpn stop"));
        }
      } else {
        assert_eq!(result.unwrap(), VpnStatus::default());
      }
      server.await.unwrap();
    }
  }
}

#[test]
fn legacy_status_ids_are_stable_and_do_not_claim_multi_connection_support() {
  for (connection_id, container_name, expected) in [
    (Some("saved"), Some("container"), "saved"),
    (None, Some("container"), "container"),
    (None, None, "legacy"),
  ] {
    let status = VpnStatus {
      connection_id: connection_id.map(str::to_owned),
      container_name: container_name.map(str::to_owned),
      running: true,
      state: VpnState::Connected,
      ..VpnStatus::default()
    };
    let leaf = normalize_status(status.clone());
    let snapshot = Response {
      status,
      snapshot: None,
    }
    .snapshot();
    assert!(!snapshot.supports_multiple);
    assert_eq!(snapshot.connections, vec![leaf]);
    assert_eq!(snapshot.connections[0].vpn_id.as_deref(), Some(expected));
  }
  let legacy: ServerMessage = serde_json::from_value(serde_json::json!({
    "type":"vpn_status", "status": { "endpoint":null,"container_name":null,"running":false,"connection_id":null,"state":"stopped" }
  })).unwrap();
  assert!(matches!(
    legacy,
    ServerMessage::VpnStatus { snapshot: None, .. }
  ));
  assert_eq!(
    Response {
      status: VpnStatus::default(),
      snapshot: None
    }
    .snapshot(),
    VpnSnapshot {
      connections: vec![],
      supports_multiple: false,
      supported_providers: vec![VpnProvider::Openconnect],
    }
  );
}
