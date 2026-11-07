//! The broker exposes errors before handing a dedicated socket to service bytes.

use super::*;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::{JoinHandle, JoinSet};

pub(super) fn spawn(
  fixture: &Fixture,
  observe: bool,
  failure: &'static str,
) -> JoinHandle<ctl_ipc::SshTarget> {
  let listener = UnixListener::bind(fixture.0.join("ctld.sock")).unwrap();
  let directory = fixture.0.clone();
  tokio::spawn(async move {
    tokio::time::timeout(Duration::from_secs(20), async {
      let (mut control, _) = listener.accept().await.unwrap();
      handshake(&mut control).await;
      let target = master(&mut control, &directory, observe).await;
      if !observe {
        return target;
      }
      let mut services = JoinSet::new();
      let mut count = 0;
      loop {
        tokio::select! {
          closed = ctl_ipc::read_frame::<_, ClientMessage>(&mut control) => {
            assert!(closed.unwrap().is_none(), "unexpected persistent broker request");
            break;
          },
          accepted = listener.accept() => {
            let (stream, _) = accepted.unwrap();
            count += 1;
            fs::write(directory.join("service-opens"), count.to_string()).unwrap();
            let directory = directory.clone();
            let target = target.clone();
            services.spawn(async move { service(stream, &directory, &target, failure).await });
          },
          finished = services.join_next(), if !services.is_empty() => {
            finished.unwrap().unwrap();
          },
        }
      }
      while let Some(finished) = services.join_next().await {
        finished.unwrap();
      }
      target
    })
    .await
    .unwrap()
  })
}

async fn handshake(stream: &mut UnixStream) {
  assert!(matches!(
    ctl_ipc::read_frame::<_, ClientMessage>(stream).await.unwrap(),
    Some(ClientMessage::Handshake { protocol })
      if protocol.negotiate(ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS).is_some()
  ));
  ctl_ipc::write_frame(
    stream,
    &ServerMessage::HandshakeAccepted {
      protocol_version: ctl_ipc::PROTOCOL_VERSION,
    },
  )
  .await
  .unwrap();
}

async fn master(
  stream: &mut UnixStream,
  directory: &std::path::Path,
  observe: bool,
) -> ctl_ipc::SshTarget {
  let observed = if observe {
    let Some(ClientMessage::MasterStatus { target }) = ctl_ipc::read_frame(stream).await.unwrap()
    else {
      panic!("expected a passive managed master observation");
    };
    ctl_ipc::write_frame(stream, &ServerMessage::AuthenticationRequired)
      .await
      .unwrap();
    Some(target)
  } else {
    None
  };
  let Some(ClientMessage::EnsureMaster { target }) = ctl_ipc::read_frame(stream).await.unwrap()
  else {
    panic!("expected one managed master request");
  };
  if let Some(observed) = observed {
    assert_eq!(target, observed);
  }
  ctl_ipc::write_frame(
    stream,
    &ServerMessage::MasterReady {
      control_path: directory.join("master"),
    },
  )
  .await
  .unwrap();
  target
}

async fn service(
  mut stream: UnixStream,
  directory: &std::path::Path,
  expected: &ctl_ipc::SshTarget,
  failure: &str,
) {
  handshake(&mut stream).await;
  assert!(matches!(
    ctl_ipc::read_frame::<_, ClientMessage>(&mut stream).await.unwrap(),
    Some(ClientMessage::OpenService { target, control_path, service })
      if &target == expected && control_path == directory.join("master")
        && service == ctl_proto::transport::Service::Ctmux
  ));
  if !directory.join("home/.tokn/ctl/current").exists() {
    ctl_ipc::write_frame(
      &mut stream,
      &ServerMessage::Error {
        code: failure.into(),
        message: "The remote agent needs compatible shared SSH services.".into(),
      },
    )
    .await
    .unwrap();
    return;
  }
  ctl_ipc::write_frame(&mut stream, &ServerMessage::ServiceOpened)
    .await
    .unwrap();
  stream
    .write_all(&fs::read(directory.join("upgraded-transport")).unwrap())
    .await
    .unwrap();
  let mut input = Vec::new();
  for _ in 0..3 {
    let request: serde_json::Value = ctl_ipc::read_frame(&mut stream).await.unwrap().unwrap();
    input.extend(frame(&request));
  }
  // Capture the complete request before the final response lets the CLI exit.
  fs::write(directory.join("broker-service-input"), input).unwrap();
  stream
    .write_all(&fs::read(directory.join("session-list")).unwrap())
    .await
    .unwrap();
  let mut remaining = Vec::new();
  stream.read_to_end(&mut remaining).await.unwrap();
  assert_eq!(remaining, [] as [u8; 0]);
}
