//! Datagram-preserving UDP forwarding over an authenticated SSH channel.

use std::io;
use std::net::IpAddr;
use std::time::Duration;

use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

pub const PROTOCOL_BUILD: u16 = 1;
pub const CONTRACT_V1_0_1: ProtocolVersion = ProtocolVersion::new(1, 0, 1);
pub const PROTOCOL_VERSION: ProtocolVersion = CONTRACT_V1_0_1;
pub const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[CONTRACT_V1_0_1];
pub const PREFACE: &[u8] = b"ctl-udp-forwarding\n";
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest UDP payload supported by both IPv4 and IPv6.
pub const MAX_DATAGRAM_BYTES: usize = 65_507;
const MAX_FRAME_BYTES: usize = 8192;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
  pub host: String,
  pub port: u16,
}

impl Request {
  /// Validates a literal IP or DNS destination before resolving or binding.
  ///
  /// # Errors
  /// Rejects empty, malformed, or unbounded destinations and port zero.
  pub fn validate(&self) -> io::Result<()> {
    if self.port == 0 || !valid_host(&self.host) {
      return Err(invalid("Invalid UDP forwarding destination"));
    }
    Ok(())
  }
}

fn valid_host(value: &str) -> bool {
  if value.parse::<IpAddr>().is_ok() {
    return true;
  }
  if value.is_empty() || value.len() > 253 || !value.is_ascii() {
    return false;
  }
  let name = value.strip_suffix('.').unwrap_or(value);
  !name.is_empty()
    && name.split('.').all(|label| {
      !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
          .bytes()
          .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
  Ready,
  Error { message: String },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
  protocol_version: ProtocolVersion,
}

#[must_use]
pub fn protocol_offer() -> ProtocolOffer {
  ProtocolOffer::new(
    PROTOCOL_BUILD,
    PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
  )
}

/// Offers supported contracts before accepting a destination or datagram.
///
/// # Errors
/// Rejects malformed or unadvertised contract selections.
pub async fn accept_contract(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<ProtocolVersion> {
  let offer = protocol_offer();
  write(writer, &offer).await?;
  let selection: Selection = read(reader).await?;
  if !offer.accepts(selection.protocol_version) {
    return Err(invalid("No shared published UDP forwarding contract"));
  }
  Ok(selection.protocol_version)
}

/// Selects the highest explicitly implemented shared forwarding contract.
///
/// # Errors
/// Rejects invalid offers or peers with no shared contract.
pub async fn negotiate_contract(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<ProtocolVersion> {
  let offer: ProtocolOffer = read(reader).await?;
  let selected = offer
    .negotiate(SUPPORTED_PROTOCOL_VERSIONS)
    .ok_or_else(|| invalid("No shared published UDP forwarding contract"))?;
  write(
    writer,
    &Selection {
      protocol_version: selected,
    },
  )
  .await?;
  Ok(selected)
}

/// Reads one bounded setup message without consuming any datagrams.
///
/// # Errors
/// Rejects oversized, incomplete, or malformed JSON frames.
pub async fn read<T: DeserializeOwned>(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<T> {
  let size = reader.read_u32().await? as usize;
  if size == 0 || size > MAX_FRAME_BYTES {
    return Err(invalid("Invalid UDP forwarding setup frame size"));
  }
  let mut bytes = vec![0; size];
  reader.read_exact(&mut bytes).await?;
  serde_json::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Writes and flushes one bounded setup message.
///
/// # Errors
/// Returns encoding, frame-size, or transport errors.
pub async fn write<T: Serialize>(
  writer: &mut (impl AsyncWrite + Unpin),
  value: &T,
) -> io::Result<()> {
  let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
  if bytes.len() > MAX_FRAME_BYTES {
    return Err(invalid("UDP forwarding setup frame is too large"));
  }
  writer
    .write_u32(u32::try_from(bytes.len()).map_err(io::Error::other)?)
    .await?;
  writer.write_all(&bytes).await?;
  writer.flush().await
}

/// Reads one complete UDP payload, including an empty datagram.
///
/// # Errors
/// Rejects oversized payloads and truncated frames. A clean frame-boundary EOF
/// returns `None`; EOF partway through a header or payload is an error.
pub async fn read_datagram(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Option<Vec<u8>>> {
  let mut header = [0; 4];
  if reader.read(&mut header[..1]).await? == 0 {
    return Ok(None);
  }
  reader.read_exact(&mut header[1..]).await?;
  let size = u32::from_be_bytes(header) as usize;
  if size > MAX_DATAGRAM_BYTES {
    return Err(invalid("UDP forwarding datagram is too large"));
  }
  let mut bytes = vec![0; size];
  reader.read_exact(&mut bytes).await?;
  Ok(Some(bytes))
}

/// Writes one framed UDP payload and flushes its boundary.
///
/// # Errors
/// Rejects oversized payloads and reports transport errors.
pub async fn write_datagram(
  writer: &mut (impl AsyncWrite + Unpin),
  bytes: &[u8],
) -> io::Result<()> {
  if bytes.len() > MAX_DATAGRAM_BYTES {
    return Err(invalid("UDP forwarding datagram is too large"));
  }
  writer
    .write_u32(u32::try_from(bytes.len()).map_err(io::Error::other)?)
    .await?;
  writer.write_all(bytes).await?;
  writer.flush().await
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn validates_ip_and_dns_destinations_without_shell_syntax() {
    for host in [
      "127.0.0.1",
      "::1",
      "localhost",
      "dns.example.",
      "a-b.example",
    ] {
      assert!(
        Request {
          host: host.into(),
          port: 53
        }
        .validate()
        .is_ok()
      );
    }
    for host in [
      "", "-host", "host-", "a..b", "a/b", "a b", "a;id", "[::1]", "a:53",
    ] {
      assert!(
        Request {
          host: host.into(),
          port: 53
        }
        .validate()
        .is_err()
      );
    }
    assert!(
      Request {
        host: "localhost".into(),
        port: 0
      }
      .validate()
      .is_err()
    );
  }

  #[tokio::test]
  async fn datagrams_preserve_boundaries_empty_payloads_and_maximum_size() {
    let datagrams = [vec![], vec![0, 255, 10], vec![17; MAX_DATAGRAM_BYTES]];
    let mut bytes = Vec::new();
    for datagram in &datagrams {
      write_datagram(&mut bytes, datagram).await.unwrap();
    }
    let mut reader = bytes.as_slice();
    for datagram in datagrams {
      assert_eq!(read_datagram(&mut reader).await.unwrap(), Some(datagram));
    }
    assert_eq!(read_datagram(&mut reader).await.unwrap(), None);
    assert!(
      write_datagram(&mut Vec::new(), &vec![0; MAX_DATAGRAM_BYTES + 1])
        .await
        .is_err()
    );
  }

  #[tokio::test]
  async fn rejects_unbounded_and_truncated_frames() {
    for bytes in [vec![0], vec![0, 0, 0, 1], u32::MAX.to_be_bytes().to_vec()] {
      assert!(read_datagram(&mut bytes.as_slice()).await.is_err());
    }
    assert!(
      read::<Request>(&mut u32::MAX.to_be_bytes().as_slice())
        .await
        .is_err()
    );
    assert!(
      serde_json::from_str::<Request>(r#"{"host":"localhost","port":53,"command":"id"}"#).is_err()
    );
  }

  #[tokio::test]
  async fn negotiation_precedes_destination_and_preserves_following_bytes() {
    let (client, server) = tokio::io::duplex(4096);
    let (mut server_reader, mut server_writer) = tokio::io::split(server);
    let server = tokio::spawn(async move {
      assert_eq!(
        accept_contract(&mut server_reader, &mut server_writer)
          .await
          .unwrap(),
        PROTOCOL_VERSION
      );
      let request: Request = read(&mut server_reader).await.unwrap();
      assert_eq!(request.host, "localhost");
      assert_eq!(
        read_datagram(&mut server_reader).await.unwrap(),
        Some(vec![0, 255])
      );
    });
    let (mut reader, mut writer) = tokio::io::split(client);
    assert_eq!(
      negotiate_contract(&mut reader, &mut writer).await.unwrap(),
      PROTOCOL_VERSION
    );
    write(
      &mut writer,
      &Request {
        host: "localhost".into(),
        port: 53,
      },
    )
    .await
    .unwrap();
    write_datagram(&mut writer, &[0, 255]).await.unwrap();
    server.await.unwrap();
  }

  #[tokio::test]
  async fn rejects_incompatible_contracts_before_destination() {
    let future = ProtocolVersion::new(2, 0, 2);
    let mut bytes = Vec::new();
    write(&mut bytes, &ProtocolOffer::new(2, future, &[future]))
      .await
      .unwrap();
    let mut output = Vec::new();
    assert!(
      negotiate_contract(&mut bytes.as_slice(), &mut output)
        .await
        .is_err()
    );
    assert_eq!(output, [] as [u8; 0]);
    let mut bytes = Vec::new();
    write(
      &mut bytes,
      &Selection {
        protocol_version: future,
      },
    )
    .await
    .unwrap();
    assert!(
      accept_contract(&mut bytes.as_slice(), &mut Vec::new())
        .await
        .is_err()
    );
  }
}
