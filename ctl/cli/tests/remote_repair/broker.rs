//! The broker exposes errors before handing a dedicated socket to service bytes.

use super::*;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::{JoinHandle, JoinSet};

pub(super) fn spawn(
  fixture: &Fixture,
  failure: Option<&'static str>,
) -> JoinHandle<ctl_ipc::SshTarget> {
  let listener = UnixListener::bind(fixture.0.join("ctld.sock")).unwrap();
  let directory = fixture.0.clone();
  tokio::spawn(async move {
    tokio::time::timeout(Duration::from_secs(20), async {
      let (mut control, _) = listener.accept().await.unwrap();
      handshake(&mut control).await;
      let target = master(&mut control, &directory, failure.is_some()).await;
      let Some(failure) = failure else {
        return target;
      };
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
  ctl_ipc::write_frame(
    &mut stream,
    &ServerMessage::Error {
      code: failure.into(),
      message: "The remote agent needs compatible shared SSH services.".into(),
    },
  )
  .await
  .unwrap();
}
