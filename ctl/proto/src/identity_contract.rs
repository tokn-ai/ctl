//! Contract negotiation occurs before environment metadata or service requests.

use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

const MAX_FRAME_BYTES: usize = 8192;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
  protocol_version: ProtocolVersion,
}

/// Advertises supported identity contracts and accepts the client's selection.
///
/// # Errors
/// Rejects oversized frames, invalid metadata and unadvertised selections.
pub async fn accept_identity_contract(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<ProtocolVersion> {
  let offer = super::identity_protocol_offer();
  write(writer, &offer).await?;
  let selection: Selection = read(reader).await?;
  if !offer.accepts(selection.protocol_version) {
    return Err(incompatible());
  }
  Ok(selection.protocol_version)
}

/// Chooses the highest explicitly supported identity contract on this channel.
///
/// # Errors
/// Rejects malformed offers and peers without an implemented common contract.
pub async fn negotiate_identity_contract(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<ProtocolVersion> {
  let offer: ProtocolOffer = read(reader).await?;
  let selected = offer
    .negotiate(super::IDENTITY_SUPPORTED_PROTOCOL_VERSIONS)
    .ok_or_else(incompatible)?;
  write(
    writer,
    &Selection {
      protocol_version: selected,
    },
  )
  .await?;
  Ok(selected)
}

fn incompatible() -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidData,
    "no shared published ctl identity contract",
  )
}

async fn read<T: DeserializeOwned>(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<T> {
  let size = reader.read_u32().await? as usize;
  if size == 0 || size > MAX_FRAME_BYTES {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid identity negotiation frame size",
    ));
  }
  let mut bytes = vec![0; size];
  reader.read_exact(&mut bytes).await?;
  serde_json::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

async fn write<T: Serialize>(writer: &mut (impl AsyncWrite + Unpin), value: &T) -> io::Result<()> {
  let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
  if bytes.len() > MAX_FRAME_BYTES {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "identity negotiation frame is too large",
    ));
  }
  writer
    .write_u32(u32::try_from(bytes.len()).map_err(io::Error::other)?)
    .await?;
  writer.write_all(&bytes).await?;
  writer.flush().await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn identity_negotiation_leaves_following_service_bytes_intact() {
    let (mut client, mut server) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(&mut server);
      assert_eq!(
        accept_identity_contract(&mut reader, &mut writer)
          .await
          .unwrap(),
        super::super::IDENTITY_PROTOCOL_VERSION
      );
      writer.write_all(b"identity then service").await.unwrap();
    });
    let (mut reader, mut writer) = tokio::io::split(&mut client);
    assert_eq!(
      negotiate_identity_contract(&mut reader, &mut writer)
        .await
        .unwrap(),
      super::super::IDENTITY_PROTOCOL_VERSION
    );
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).await.unwrap();
    assert_eq!(rest, b"identity then service");
    server.await.unwrap();
  }

  #[tokio::test]
  async fn rejects_future_major_before_sending_any_service_input() {
    let future = ProtocolVersion::new(2, 0, 15);
    let mut bytes = Vec::new();
    write(&mut bytes, &ProtocolOffer::new(15, future, &[future]))
      .await
      .unwrap();
    let mut output = Vec::new();
    assert!(
      negotiate_identity_contract(&mut bytes.as_slice(), &mut output)
        .await
        .is_err()
    );
    assert_eq!(output, [] as [u8; 0]);
  }

  #[tokio::test]
  async fn server_rejects_unadvertised_selection() {
    let mut request = Vec::new();
    write(
      &mut request,
      &Selection {
        protocol_version: ProtocolVersion::new(1, 1, 15),
      },
    )
    .await
    .unwrap();
    assert!(
      accept_identity_contract(&mut request.as_slice(), &mut Vec::new())
        .await
        .is_err()
    );
  }
}
