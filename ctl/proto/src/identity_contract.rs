//! Contract negotiation occurs before environment metadata or service requests.

use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

const MAX_FRAME_BYTES: usize = 8192;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentitySelection {
  pub protocol_version: ProtocolVersion,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub client_identity: Option<ctl_core::presence::ClientIdentity>,
}

type Selection = IdentitySelection;

/// Advertises supported identity contracts and accepts the client's selection.
///
/// # Errors
/// Rejects oversized frames, invalid metadata and unadvertised selections.
pub async fn accept_identity_contract(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<ProtocolVersion> {
  Ok(
    accept_identity_contract_with_client(reader, writer)
      .await?
      .protocol_version,
  )
}

/// Accepts optional process metadata only under the negotiated presence contract.
///
/// # Errors
/// Rejects malformed labels, oversized frames and unsupported selections.
pub async fn accept_identity_contract_with_client(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<IdentitySelection> {
  let offer = super::identity_protocol_offer();
  write(writer, &offer).await?;
  let selection: Selection = read(reader).await?;
  if !offer.accepts(selection.protocol_version) {
    return Err(incompatible());
  }
  if selection.client_identity.as_ref().is_some_and(|client| {
    !client.validate() || selection.protocol_version != super::IDENTITY_CONTRACT_V1_1_4
  }) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "Invalid client presence identity",
    ));
  }
  Ok(selection)
}

/// Chooses the highest explicitly supported identity contract on this channel.
///
/// # Errors
/// Rejects malformed offers and peers without an implemented common contract.
pub async fn negotiate_identity_contract(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<ProtocolVersion> {
  negotiate_identity_contract_with_client(reader, writer, None).await
}

/// Includes a client label only when both peers explicitly support it.
///
/// # Errors
/// Rejects malformed labels, oversized frames and unsupported offers.
pub async fn negotiate_identity_contract_with_client(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
  client_identity: Option<&ctl_core::presence::ClientIdentity>,
) -> io::Result<ProtocolVersion> {
  if client_identity.is_some_and(|client| !client.validate()) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "Invalid client presence identity",
    ));
  }
  let offer: ProtocolOffer = read(reader).await?;
  let selected = offer
    .negotiate(super::IDENTITY_SUPPORTED_PROTOCOL_VERSIONS)
    .ok_or_else(incompatible)?;
  write(
    writer,
    &Selection {
      protocol_version: selected,
      client_identity: client_identity
        .filter(|_| selected == super::IDENTITY_CONTRACT_V1_1_4)
        .cloned(),
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

  fn client_identity() -> ctl_core::presence::ClientIdentity {
    ctl_core::presence::ClientIdentity {
      instance_id: "fixture-instance".into(),
      device_name: Some("Fixture laptop".into()),
      client_name: "ctmux-app".into(),
      client_version: "0.1.2".into(),
    }
  }

  #[tokio::test]
  async fn presence_negotiation_returns_client_labels_and_leaves_service_bytes_intact() {
    let (mut client, mut server) = tokio::io::duplex(4096);
    let expected = client_identity();
    let accepted = expected.clone();
    let server = tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(&mut server);
      let selection = accept_identity_contract_with_client(&mut reader, &mut writer)
        .await
        .unwrap();
      assert_eq!(
        selection.protocol_version,
        super::super::IDENTITY_CONTRACT_V1_1_4
      );
      assert_eq!(selection.client_identity, Some(accepted));
      let mut bytes = [0; 5];
      reader.read_exact(&mut bytes).await.unwrap();
      assert_eq!(bytes, [0, 255, 1, 2, 3]);
    });
    let (mut reader, mut writer) = tokio::io::split(&mut client);
    negotiate_identity_contract_with_client(&mut reader, &mut writer, Some(&expected))
      .await
      .unwrap();
    writer.write_all(&[0, 255, 1, 2, 3]).await.unwrap();
    server.await.unwrap();
  }

  #[tokio::test]
  async fn legacy_selection_is_byte_identical_even_when_client_metadata_is_available() {
    let version = super::super::IDENTITY_CONTRACT_V1_0_3;
    let mut input = Vec::new();
    write(&mut input, &ProtocolOffer::new(3, version, &[version]))
      .await
      .unwrap();
    let mut output = Vec::new();
    assert_eq!(
      negotiate_identity_contract_with_client(
        &mut input.as_slice(),
        &mut output,
        Some(&client_identity())
      )
      .await
      .unwrap(),
      version
    );
    let mut expected = Vec::new();
    write(
      &mut expected,
      &serde_json::json!({"protocol_version": version}),
    )
    .await
    .unwrap();
    assert_eq!(output, expected);
  }

  #[tokio::test]
  async fn metadata_on_an_old_selection_and_invalid_labels_are_rejected() {
    for (version, client) in [
      (super::super::IDENTITY_CONTRACT_V1_0_3, client_identity()),
      (
        super::super::IDENTITY_CONTRACT_V1_1_4,
        ctl_core::presence::ClientIdentity {
          client_name: "unsafe\nlabel".into(),
          ..client_identity()
        },
      ),
    ] {
      let mut input = Vec::new();
      write(
        &mut input,
        &Selection {
          protocol_version: version,
          client_identity: Some(client),
        },
      )
      .await
      .unwrap();
      assert!(
        accept_identity_contract_with_client(&mut input.as_slice(), &mut Vec::new())
          .await
          .is_err()
      );
    }
  }

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
        client_identity: None,
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
