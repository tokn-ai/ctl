//! Passive version inspection. Never bootstraps a daemon or touches sessions.
use super::{
  LocalControlClientMessage, LocalControlErrorCode, LocalControlServerMessage, connect,
  control_socket_path,
};
use ctl_core::component::ComponentBuildInfo;
use std::io;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct ComponentStatus {
  pub restart_supported: bool,
  pub build: Option<ComponentBuildInfo>,
  pub version: Option<String>,
  pub protocol_version: Option<u16>,
  pub control_protocol_version: Option<u16>,
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
      protocol_version: super::LOCAL_CONTROL_PROTOCOL_VERSION,
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
      restart_supported,
      build,
      data_protocol_version,
      ..
    }) => Ok(Some(ComponentStatus {
      restart_supported,
      version: build.as_ref().map(|build| build.version.clone()),
      build,
      protocol_version: data_protocol_version,
      control_protocol_version: Some(protocol_version),
      protocol_mismatch: protocol_version != super::LOCAL_CONTROL_PROTOCOL_VERSION,
    })),
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
      protocol_version: ctmux_proto::PROTOCOL_VERSION,
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
      server_version,
      build,
      ..
    }) => {
      let prior = control_status.unwrap_or_default();
      Ok(Some(ComponentStatus {
        restart_supported: prior.restart_supported,
        build: build.or(prior.build),
        version: Some(server_version),
        protocol_version: Some(protocol_version),
        control_protocol_version: prior.control_protocol_version,
        protocol_mismatch: prior.protocol_mismatch
          || protocol_version != ctmux_proto::PROTOCOL_VERSION,
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
