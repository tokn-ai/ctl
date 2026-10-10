//! Passive task owner inspection, independent of task execution.
use ctl_core::component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo};
use ctl_task_proto::{ClientMessage, ServerMessage, control, read_frame, write_frame};
use std::io;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ComponentStatus {
  pub build: Option<ComponentBuildInfo>,
  pub protocols: Vec<ProtocolInfo>,
  pub protocol_version: Option<ctl_core::protocol::ProtocolVersion>,
  /// The control protocol accepted by a successful metadata exchange.
  pub control_protocol_version: Option<ctl_core::protocol::ProtocolVersion>,
  /// A typed rejection establishes incompatibility without reporting a version.
  pub protocol_mismatch: bool,
}

/// Reads a running task owner without starting it or listing/changing tasks.
///
/// # Errors
/// Returns connection, timeout, and malformed-response errors.
pub async fn component_status() -> io::Result<Option<ComponentStatus>> {
  component_status_at(&super::socket_path()).await
}

/// Reads one selected task owner without starting it or listing/changing tasks.
///
/// # Errors
/// Returns connection, timeout, and malformed-response errors.
pub async fn component_status_at(path: &Path) -> io::Result<Option<ComponentStatus>> {
  tokio::time::timeout(Duration::from_secs(3), probe(path))
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "ctl-taskd version check timed out"))?
}

async fn probe(path: &Path) -> io::Result<Option<ComponentStatus>> {
  let mut stream = match super::connect(path).await {
    Ok(stream) => stream,
    Err(error) if absent(&error) => return Ok(None),
    Err(error) => return Err(error),
  };
  write_frame(
    &mut stream,
    &control::ClientMessage::ComponentStatus {
      protocol: control::protocol_offer(),
    },
  )
  .await
  .map_err(io::Error::other)?;
  if let Ok(Some(control::ServerMessage::ComponentStatus {
    build,
    protocol_version,
    data_protocol_version,
    protocols,
  })) = read_frame(&mut stream).await
  {
    let info = ComponentInfo { build, protocols };
    let compatible = info.is_valid()
      && control::protocol_offer().accepts(protocol_version)
      && info
        .protocols
        .iter()
        .any(|protocol| protocol.name == "task_control" && protocol.supports(protocol_version))
      && info.protocols.iter().any(|protocol| {
        protocol.name == "task"
          && protocol.version == data_protocol_version
          && protocol
            .negotiate(ctl_task_proto::SUPPORTED_PROTOCOL_VERSIONS)
            .is_some()
      });
    return Ok(Some(ComponentStatus {
      build: Some(info.build),
      protocols: info.protocols,
      protocol_version: Some(data_protocol_version),
      control_protocol_version: Some(protocol_version),
      protocol_mismatch: !compatible,
    }));
  }
  // Older task owners close unknown control requests. A fresh data handshake
  // can still expose their protocol; do not mistake missing build data for current.
  drop(stream);
  let mut stream = super::connect(path).await?;
  write_frame(
    &mut stream,
    &ClientMessage::Handshake {
      protocol: ctl_task_proto::protocol_offer(),
      client_name: "ctmux-about".into(),
    },
  )
  .await
  .map_err(io::Error::other)?;
  match read_frame(&mut stream).await.map_err(io::Error::other)? {
    Some(ServerMessage::HandshakeAccepted {
      protocol_version,
      protocols,
    }) => {
      let compatible = ctl_task_proto::protocol_offer().accepts(protocol_version)
        && ctl_core::component::protocols_are_valid(&protocols)
        && protocols
          .iter()
          .any(|protocol| protocol.name == "task" && protocol.supports(protocol_version));
      Ok(Some(ComponentStatus {
        build: None,
        protocols,
        protocol_version: Some(protocol_version),
        control_protocol_version: None,
        protocol_mismatch: !compatible,
      }))
    }
    Some(ServerMessage::Error {
      code: ctl_task_proto::ErrorCode::ProtocolVersionMismatch,
      ..
    }) => Ok(Some(ComponentStatus {
      build: None,
      protocols: Vec::new(),
      protocol_version: None,
      control_protocol_version: None,
      protocol_mismatch: true,
    })),
    Some(ServerMessage::Error { message, .. }) => Err(io::Error::other(message)),
    _ => Err(io::Error::other(
      "ctl-taskd returned invalid version metadata",
    )),
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
    let path = std::env::temp_dir().join(format!("task-about-shared-{}.sock", std::process::id()));
    let listener = UnixListener::bind(&path).unwrap();
    let data_latest = ProtocolVersion::new(1, 1, 6);
    let control_latest = ProtocolVersion::new(1, 1, 3);
    let protocols = vec![
      ProtocolInfo::new(
        "task",
        6,
        data_latest,
        &[ctl_task_proto::PROTOCOL_VERSION, data_latest],
      ),
      ProtocolInfo::new(
        "task_control",
        3,
        control_latest,
        &[control::PROTOCOL_VERSION, control_latest],
      ),
    ];
    let advertised = protocols.clone();
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        read_frame::<_, control::ClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(control::ClientMessage::ComponentStatus { .. })
      ));
      write_frame(
        &mut stream,
        &control::ServerMessage::ComponentStatus {
          build: ctl_core::component::build_info(),
          protocol_version: control::PROTOCOL_VERSION,
          data_protocol_version: data_latest,
          protocols: advertised,
        },
      )
      .await
      .unwrap();
      assert!(
        read_frame::<_, ClientMessage>(&mut stream)
          .await
          .unwrap()
          .is_none()
      );
    });
    let status = component_status_at(&path).await.unwrap().unwrap();
    assert!(!status.protocol_mismatch);
    assert_eq!(status.protocol_version, Some(data_latest));
    assert_eq!(
      status.control_protocol_version,
      Some(control::PROTOCOL_VERSION)
    );
    assert_eq!(status.protocols, protocols);
    server.await.unwrap();
    std::fs::remove_file(path).unwrap();
  }

  #[tokio::test]
  async fn absent_probe_does_not_start_a_task_owner() {
    let path = std::env::temp_dir().join(format!("task-about-{}.sock", std::process::id()));
    assert!(probe(&path).await.unwrap().is_none());
    assert!(!path.exists());
  }

  #[tokio::test]
  async fn metadata_query_works_even_when_task_protocol_differs() {
    let path = std::env::temp_dir().join(format!("task-about-version-{}.sock", std::process::id()));
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        read_frame::<_, control::ClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(control::ClientMessage::ComponentStatus { .. })
      ));
      write_frame(
        &mut stream,
        &control::ServerMessage::ComponentStatus {
          build: ctl_core::component::build_info(),
          protocol_version: control::PROTOCOL_VERSION,
          data_protocol_version: ctl_core::protocol::ProtocolVersion::new(2, 0, 5),
          protocols: vec![
            ProtocolInfo::new(
              "task",
              5,
              ctl_core::protocol::ProtocolVersion::new(2, 0, 5),
              &[ctl_core::protocol::ProtocolVersion::new(2, 0, 5)],
            ),
            control::protocol_info(),
          ],
        },
      )
      .await
      .unwrap();
      assert!(
        read_frame::<_, ClientMessage>(&mut stream)
          .await
          .unwrap()
          .is_none()
      );
    });
    let status = probe(&path).await.unwrap().unwrap();
    assert_eq!(
      status.protocol_version,
      Some(ctl_core::protocol::ProtocolVersion::new(2, 0, 5))
    );
    assert_eq!(
      status.control_protocol_version,
      Some(control::PROTOCOL_VERSION)
    );
    assert!(status.protocol_mismatch);
    server.await.unwrap();
    std::fs::remove_file(path).unwrap();
  }

  async fn probe_legacy_reply(
    name: &str,
    reply: ServerMessage,
  ) -> io::Result<Option<ComponentStatus>> {
    let path = std::env::temp_dir().join(format!("task-about-{name}-{}.sock", std::process::id()));
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        read_frame::<_, control::ClientMessage>(&mut stream)
          .await
          .unwrap(),
        Some(control::ClientMessage::ComponentStatus { .. })
      ));
      // Legacy control failures lack a typed code. Their text cannot prove a mismatch.
      write_frame(
        &mut stream,
        &control::ServerMessage::Error {
          message: "protocol version mismatch 999".into(),
        },
      )
      .await
      .unwrap();
      drop(stream);
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        read_frame::<_, ClientMessage>(&mut stream).await.unwrap(),
        Some(ClientMessage::Handshake { .. })
      ));
      write_frame(&mut stream, &reply).await.unwrap();
      assert!(
        read_frame::<_, ClientMessage>(&mut stream)
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
    let status = probe_legacy_reply(
      "rejected",
      ServerMessage::Error {
        code: ctl_task_proto::ErrorCode::ProtocolVersionMismatch,
        message: "The version 999 in this message is not structured metadata".into(),
      },
    )
    .await
    .unwrap()
    .unwrap();
    assert!(status.protocol_mismatch);
    assert!(status.protocol_version.is_none());
    assert!(status.control_protocol_version.is_none());
    assert!(status.build.is_none());
  }

  #[tokio::test]
  async fn legacy_control_error_text_does_not_override_an_accepted_data_handshake() {
    let status = probe_legacy_reply(
      "accepted",
      ServerMessage::HandshakeAccepted {
        protocol_version: ctl_task_proto::PROTOCOL_VERSION,
        protocols: vec![ctl_task_proto::protocol_info()],
      },
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!status.protocol_mismatch);
    assert_eq!(
      status.protocol_version,
      Some(ctl_task_proto::PROTOCOL_VERSION)
    );
    assert!(status.control_protocol_version.is_none());
    assert!(status.build.is_none());
  }

  #[tokio::test]
  async fn unrelated_data_errors_are_not_classified_from_their_text() {
    let error = probe_legacy_reply(
      "internal",
      ServerMessage::Error {
        code: ctl_task_proto::ErrorCode::Internal,
        message: "protocol version mismatch 999".into(),
      },
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "protocol version mismatch 999");
  }
}
