//! Passive version inspection. Never bootstraps a daemon or touches sessions.
use super::{
  LocalControlClientMessage, LocalControlErrorCode, LocalControlServerMessage, connect,
  control_socket_path,
};
use ctl_core::component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo};
use std::io;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct ComponentStatus {
  pub restart_supported: bool,
  pub build: Option<ComponentBuildInfo>,
  pub protocols: Vec<ProtocolInfo>,
  pub version: Option<String>,
  pub protocol_version: Option<ctl_core::protocol::ProtocolVersion>,
  pub control_protocol_version: Option<ctl_core::protocol::ProtocolVersion>,
  /// A typed rejection establishes incompatibility without reporting a version.
  pub protocol_mismatch: bool,
}

/// Reads the selected running daemon's own metadata without starting it.
///
/// # Errors
/// Returns connection, timeout, or malformed-response errors. Missing endpoints
/// return `None`; a legacy control endpoint falls back to the data handshake.
pub async fn component_status() -> io::Result<Option<ComponentStatus>> {
  tokio::time::timeout(Duration::from_secs(3), probe(&super::socket_path()))
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "ctmuxd version check timed out"))?
}

async fn probe_control(path: &Path) -> io::Result<Option<ComponentStatus>> {
  let mut stream = match connect(&control_socket_path(path)?).await {
    Ok(stream) => stream,
    Err(error) if absent(&error) => return Ok(None),
    Err(error) => return Err(error),
  };
  super::write_local_control_frame(
    &mut stream,
    &LocalControlClientMessage::Handshake {
      protocol: super::local_control_offer(),
    },
  )
  .await
  .map_err(io::Error::other)?;
  match super::read_local_control_frame(&mut stream)
    .await
    .map_err(io::Error::other)?
  {
    Some(LocalControlServerMessage::HandshakeAccepted {
      protocol_version,
      protocols,
      restart_supported,
      build,
      data_protocol_version,
      ..
    }) => {
      let metadata_valid = build.as_ref().is_none_or(|build| {
        ComponentInfo {
          build: build.clone(),
          protocols: protocols.clone(),
        }
        .is_valid()
      });
      let compatible = metadata_valid
        && ctl_core::component::protocols_are_valid(&protocols)
        && super::local_control_offer().accepts(protocol_version)
        && ctl_core::component::protocols_are_valid(&protocols)
        && protocols
          .iter()
          .any(|protocol| protocol.name == "ctmux_control" && protocol.supports(protocol_version))
        && data_protocol_version.is_none_or(|version| {
          protocols
            .iter()
            .any(|protocol| protocol.name == "ctmux" && protocol.version == version)
        })
        && protocols
          .iter()
          .find(|protocol| protocol.name == "ctmux")
          .is_none_or(|protocol| {
            protocol
              .negotiate(ctmux_proto::SUPPORTED_PROTOCOL_VERSIONS)
              .is_some()
          });
      Ok(Some(ComponentStatus {
        restart_supported,
        version: build.as_ref().map(|build| build.version.clone()),
        build,
        protocols,
        protocol_version: data_protocol_version,
        control_protocol_version: Some(protocol_version),
        protocol_mismatch: !compatible,
      }))
    }
    Some(LocalControlServerMessage::Error {
      code: LocalControlErrorCode::ProtocolVersionMismatch,
      ..
    }) => Ok(Some(ComponentStatus {
      protocol_mismatch: true,
      ..ComponentStatus::default()
    })),
    Some(LocalControlServerMessage::Error { message, .. }) => Err(io::Error::other(message)),
    _ => Err(io::Error::other(
      "ctmuxd returned invalid control version metadata",
    )),
  }
}

async fn probe(path: &Path) -> io::Result<Option<ComponentStatus>> {
  let control_status = probe_control(path).await?;
  if control_status.as_ref().is_some_and(|status| {
    status.protocol_mismatch || (status.build.is_some() && status.protocol_version.is_some())
  }) {
    return Ok(control_status);
  }
  let mut stream = match connect(path).await {
    Ok(stream) => stream,
    Err(error) if absent(&error) => return Ok(control_status),
    Err(error) => return Err(error),
  };
  ctmux_proto::write_frame(
    &mut stream,
    &ctmux_proto::ClientMessage::Handshake {
      protocol: ctmux_proto::protocol_offer(),
      client_name: "ctmux-about".into(),
      client_version: env!("CARGO_PKG_VERSION").into(),
    },
  )
  .await
  .map_err(io::Error::other)?;
  match ctmux_proto::read_frame(&mut stream)
    .await
    .map_err(io::Error::other)?
  {
    Some(ctmux_proto::ServerMessage::HandshakeAccepted {
      protocol_version,
      protocols,
      server_version,
      build,
      ..
    }) => {
      let prior = control_status.unwrap_or_default();
      let compatible = ctmux_proto::protocol_offer().accepts(protocol_version)
        && ctl_core::component::protocols_are_valid(&protocols)
        && protocols
          .iter()
          .any(|protocol| protocol.name == "ctmux" && protocol.supports(protocol_version));
      Ok(Some(ComponentStatus {
        restart_supported: prior.restart_supported,
        build: build.or(prior.build),
        protocols,
        version: Some(server_version),
        protocol_version: Some(protocol_version),
        control_protocol_version: prior.control_protocol_version,
        protocol_mismatch: prior.protocol_mismatch || !compatible,
      }))
    }
    Some(ctmux_proto::ServerMessage::Error {
      code: ctmux_proto::ErrorCode::ProtocolVersionMismatch,
      ..
    }) => Ok(Some(ComponentStatus {
      protocol_mismatch: true,
      ..control_status.unwrap_or_default()
    })),
    Some(ctmux_proto::ServerMessage::Error { message, .. }) => Err(io::Error::other(message)),
    _ => Err(io::Error::other("ctmuxd returned invalid version metadata")),
  }
}

fn absent(error: &io::Error) -> bool {
  matches!(
    error.kind(),
    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
  )
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use tokio::net::UnixListener;

  #[tokio::test]
  async fn newer_owner_is_compatible_when_it_advertises_the_local_contracts() {
    use ctl_core::protocol::ProtocolVersion;
    let path = std::env::temp_dir().join(format!("ctmux-about-shared-{}.sock", std::process::id()));
    let control_path = control_socket_path(&path).unwrap();
    let listener = UnixListener::bind(&control_path).unwrap();
    let data_latest = ProtocolVersion::new(1, 1, 15);
    let control_latest = ProtocolVersion::new(1, 1, 2);
    let protocols = vec![
      ProtocolInfo::new(
        "ctmux",
        15,
        data_latest,
        &[ctmux_proto::PROTOCOL_VERSION, data_latest],
      ),
      ProtocolInfo::new(
        "ctmux_control",
        2,
        control_latest,
        &[crate::LOCAL_CONTROL_PROTOCOL_VERSION, control_latest],
      ),
    ];
    let advertised = protocols.clone();
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        crate::read_local_control_frame::<_, LocalControlClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(LocalControlClientMessage::Handshake { .. })
      ));
      crate::write_local_control_frame(
        &mut stream,
        &LocalControlServerMessage::HandshakeAccepted {
          protocol_version: crate::LOCAL_CONTROL_PROTOCOL_VERSION,
          protocols: advertised,
          restart_supported: true,
          build: Some(ctl_core::component::build_info()),
          data_protocol_version: Some(data_latest),
          managed_sessions_supported: true,
        },
      )
      .await
      .unwrap();
      assert!(
        crate::read_local_control_frame::<_, LocalControlClientMessage>(&mut stream)
          .await
          .unwrap()
          .is_none()
      );
    });
    let status = probe(&path).await.unwrap().unwrap();
    assert!(!status.protocol_mismatch);
    assert_eq!(status.protocol_version, Some(data_latest));
    assert_eq!(
      status.control_protocol_version,
      Some(crate::LOCAL_CONTROL_PROTOCOL_VERSION)
    );
    assert_eq!(status.protocols, protocols);
    server.await.unwrap();
    std::fs::remove_file(control_path).unwrap();
  }

  #[tokio::test]
  async fn absent_probe_does_not_create_endpoints() {
    let path = std::env::temp_dir().join(format!("ctmux-about-{}.sock", std::process::id()));
    assert!(probe(&path).await.unwrap().is_none());
    assert!(!path.exists());
    assert!(!control_socket_path(&path).unwrap().exists());
  }

  #[tokio::test]
  async fn legacy_data_only_daemon_reports_observed_version_without_session_requests() {
    let path = std::env::temp_dir().join(format!("ctmux-about-legacy-{}.sock", std::process::id()));
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        ctmux_proto::read_frame::<_, ctmux_proto::ClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(ctmux_proto::ClientMessage::Handshake { .. })
      ));
      ctmux_proto::write_frame(
        &mut stream,
        &ctmux_proto::ServerMessage::HandshakeAccepted {
          protocol_version: ctmux_proto::PROTOCOL_VERSION,
          protocols: vec![ctmux_proto::protocol_info()],
          server_version: "0.0.9".into(),
          build: None,
          heartbeat_interval_ms: 1000,
          attachment_liveness_timeout_ms: 3000,
        },
      )
      .await
      .unwrap();
      assert!(
        ctmux_proto::read_frame::<_, ctmux_proto::ClientMessage>(&mut stream)
          .await
          .unwrap()
          .is_none()
      );
    });
    let status = probe(&path).await.unwrap().unwrap();
    assert_eq!(status.version.as_deref(), Some("0.0.9"));
    assert!(status.build.is_none());
    assert!(!status.protocol_mismatch);
    server.await.unwrap();
    std::fs::remove_file(path).unwrap();
  }

  async fn probe_data_reply(
    name: &str,
    reply: ctmux_proto::ServerMessage,
  ) -> io::Result<Option<ComponentStatus>> {
    let path = std::env::temp_dir().join(format!("ctmux-about-{name}-{}.sock", std::process::id()));
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        ctmux_proto::read_frame::<_, ctmux_proto::ClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(ctmux_proto::ClientMessage::Handshake { .. })
      ));
      ctmux_proto::write_frame(&mut stream, &reply).await.unwrap();
      assert!(
        ctmux_proto::read_frame::<_, ctmux_proto::ClientMessage>(&mut stream)
          .await
          .unwrap()
          .is_none()
      );
    });
    let status = probe(&path).await;
    server.await.unwrap();
    std::fs::remove_file(path).unwrap();
    status
  }

  #[tokio::test]
  async fn legacy_data_rejection_preserves_mismatch_without_inventing_version() {
    let status = probe_data_reply(
      "rejected",
      ctmux_proto::ServerMessage::Error {
        code: ctmux_proto::ErrorCode::ProtocolVersionMismatch,
        message: "The version 999 in this message is not structured metadata".into(),
      },
    )
    .await
    .unwrap()
    .unwrap();
    assert!(status.protocol_mismatch);
    assert!(status.protocol_version.is_none());
    assert!(status.control_protocol_version.is_none());
    assert!(status.version.is_none());
    assert!(status.build.is_none());
  }

  #[tokio::test]
  async fn unrelated_data_errors_are_not_classified_from_their_text() {
    let error = probe_data_reply(
      "internal",
      ctmux_proto::ServerMessage::Error {
        code: ctmux_proto::ErrorCode::Internal,
        message: "protocol version mismatch 999".into(),
      },
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "protocol version mismatch 999");
  }

  #[tokio::test]
  async fn legacy_control_rejection_preserves_mismatch_without_a_data_endpoint() {
    let path =
      std::env::temp_dir().join(format!("ctmux-about-control-{}.sock", std::process::id()));
    let control_path = control_socket_path(&path).unwrap();
    let listener = UnixListener::bind(&control_path).unwrap();
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        crate::read_local_control_frame::<_, LocalControlClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(LocalControlClientMessage::Handshake { .. })
      ));
      crate::write_local_control_frame(
        &mut stream,
        &LocalControlServerMessage::Error {
          code: LocalControlErrorCode::ProtocolVersionMismatch,
          message: "The actual control version was not supplied".into(),
        },
      )
      .await
      .unwrap();
      assert!(
        crate::read_local_control_frame::<_, LocalControlClientMessage>(&mut stream)
          .await
          .unwrap()
          .is_none()
      );
    });
    let status = probe(&path).await.unwrap().unwrap();
    assert!(status.protocol_mismatch);
    assert!(status.protocol_version.is_none());
    assert!(status.control_protocol_version.is_none());
    server.await.unwrap();
    std::fs::remove_file(control_path).unwrap();
    assert!(!path.exists());
  }
}
