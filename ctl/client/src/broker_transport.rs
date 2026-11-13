//! Fixed service streams on the broker's shared remote agent connection.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, BufReader};

use crate::{
  CoreError, RemotePlatform, RemoteService, SshInteraction, SshIo, SshTransport,
  read_negotiated_ssh_identity, ssh_startup,
};

pub(super) async fn open(
  options: &crate::SshConnectionOptions,
  interaction: &SshInteraction,
  service: RemoteService,
  identified: bool,
) -> Result<Option<SshTransport>, CoreError> {
  let SshInteraction::Brokered {
    target,
    control_path,
  } = interaction
  else {
    return Ok(None);
  };
  if options.remote_platform != RemotePlatform::Unix {
    return Ok(None);
  }
  let service = match service {
    RemoteService::Ctmux => ctl_proto::transport::Service::Ctmux,
    RemoteService::Task => ctl_proto::transport::Service::Task,
  };
  let stream =
    crate::connection::open_service(target.clone(), control_path.clone(), service).await?;
  from_stream(stream, identified).await.map(Some)
}

pub(super) async fn from_stream(
  stream: ctl_ipc::Stream,
  identified: bool,
) -> Result<SshTransport, CoreError> {
  let (reader, mut stdin) = tokio::io::split(stream);
  let mut stdout = BufReader::new(reader);
  let identity = tokio::time::timeout(
    Duration::from_secs(30),
    read_identity(&mut stdout, &mut stdin),
  )
  .await
  .map_err(|_| {
    CoreError::ReadSshPreface(io::Error::new(
      io::ErrorKind::TimedOut,
      "shared SSH service did not finish its identity handshake",
    ))
  })??;
  Ok(SshTransport {
    remote_identity: identified.then(|| Box::new(identity)),
    io: SshIo::Brokered { stdin, stdout },
  })
}

async fn read_identity(
  stdout: &mut (impl AsyncRead + Unpin),
  stdin: &mut (impl AsyncWrite + Unpin),
) -> Result<ctl_proto::RemoteIdentity, CoreError> {
  ssh_startup::Preface::default()
    .read_marker(stdout, &[ctl_proto::IDENTITY_PREFACE], b"ctl-ssh-")
    .await
    .map_err(CoreError::ReadSshPreface)?;
  read_negotiated_ssh_identity(stdout, stdin)
    .await
    .map_err(CoreError::RemoteIdentity)
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

  fn identity() -> ctl_proto::RemoteIdentity {
    ctl_proto::RemoteIdentity {
      remote_id: uuid::Uuid::new_v4().to_string(),
      agent_version: "fixture".into(),
      build: None,
      ctmux_restart_supported: false,
      bundle: None,
      protocols: ctl_proto::agent_protocols(),
    }
  }

  async fn write_service_identity<S>(stream: S, identity: ctl_proto::RemoteIdentity)
  where
    S: AsyncRead + AsyncWrite + Unpin,
  {
    let (mut reader, mut writer) = tokio::io::split(stream);
    writer.write_all(ctl_proto::IDENTITY_PREFACE).await.unwrap();
    let selection = ctl_proto::accept_identity_contract_with_client(&mut reader, &mut writer)
      .await
      .unwrap();
    assert!(selection.client_identity.is_some());
    ctl_proto::write_identity(&mut writer, &identity)
      .await
      .unwrap();
    writer.write_all(b"service bytes").await.unwrap();
    let mut discarded = Vec::new();
    reader.read_to_end(&mut discarded).await.unwrap();
    assert_eq!(discarded.as_slice(), b"");
  }

  #[tokio::test]
  async fn shared_stream_negotiates_identity_before_reading_service_data() {
    let expected = identity();
    let (stream, remote) = tokio::io::duplex(8192);
    let peer = tokio::spawn(write_service_identity(remote, expected.clone()));
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    assert_eq!(
      read_identity(&mut reader, &mut writer).await.unwrap(),
      expected
    );
    let mut payload = [0; 13];
    reader.read_exact(&mut payload).await.unwrap();
    assert_eq!(&payload, b"service bytes");
    drop(reader);
    drop(writer);
    peer.await.unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn dropping_a_shared_service_closes_only_its_owned_broker_stream() {
    for identified in [false, true] {
      let expected = identity();
      let (stream, remote) = ctl_ipc::Stream::pair().unwrap();
      let peer = tokio::spawn(write_service_identity(remote, expected.clone()));
      let mut transport = from_stream(stream, identified).await.unwrap();
      assert_eq!(
        transport.remote_identity.as_deref(),
        identified.then_some(&expected)
      );
      let mut payload = [0; 13];
      transport.read_exact(&mut payload).await.unwrap();
      assert_eq!(&payload, b"service bytes");
      drop(transport);
      tokio::time::timeout(Duration::from_secs(2), peer)
        .await
        .unwrap()
        .unwrap();
    }
  }
}
