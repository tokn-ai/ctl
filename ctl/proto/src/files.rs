//! One bounded read-only operation per independently cancellable service stream.

use std::io;
use std::time::Duration;

use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

pub const PROTOCOL_BUILD: u16 = 1;
pub const CONTRACT_V1_0_1: ProtocolVersion = ProtocolVersion::new(1, 0, 1);
pub const PROTOCOL_VERSION: ProtocolVersion = CONTRACT_V1_0_1;
pub const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[CONTRACT_V1_0_1];
pub const CONTRACT: ctl_core::component::ProtocolContract =
  ctl_core::component::ProtocolContract::new(
    "ctl_files",
    PROTOCOL_BUILD,
    PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
  );
pub const PREFACE: &[u8] = b"ctl-file-service\n";
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
pub const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_READ_BYTES: u32 = 4 * 1024 * 1024;
pub const MAX_DIRECTORY_ENTRIES: u32 = 1000;
pub const MAX_DIRECTORY_OFFSET: u32 = 100_000;
pub const MAX_PATH_BYTES: usize = 16 * 1024;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 2 * MAX_PATH_BYTES + 1024;
const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
  Metadata {
    path: String,
    cwd: Option<String>,
  },
  ReadFile {
    path: String,
    cwd: Option<String>,
    offset: u64,
    length: u32,
  },
  ListDirectory {
    path: String,
    cwd: Option<String>,
    offset: u32,
    limit: u32,
  },
  GitStatus {
    path: String,
    cwd: Option<String>,
  },
}

impl Request {
  #[must_use]
  pub fn location(&self) -> (&str, Option<&str>) {
    match self {
      Self::Metadata { path, cwd }
      | Self::ReadFile { path, cwd, .. }
      | Self::ListDirectory { path, cwd, .. }
      | Self::GitStatus { path, cwd } => (path, cwd.as_deref()),
    }
  }

  /// Rejects unbounded inputs before filesystem access or allocation.
  ///
  /// # Errors
  /// Returns invalid input for malformed paths, ranges, or page sizes.
  pub fn validate(&self) -> io::Result<()> {
    let (path, cwd) = self.location();
    if !valid_path(path) || cwd.is_some_and(|value| !valid_path(value)) {
      return Err(invalid("Invalid file path or working directory"));
    }
    match self {
      Self::ReadFile { offset, length, .. }
        if *offset > MAX_SAFE_INTEGER || *length == 0 || *length > MAX_READ_BYTES =>
      {
        Err(invalid("File read exceeds the supported range"))
      }
      Self::ListDirectory { offset, limit, .. }
        if *offset > MAX_DIRECTORY_OFFSET || *limit == 0 || *limit > MAX_DIRECTORY_ENTRIES =>
      {
        Err(invalid("Directory page exceeds the supported range"))
      }
      _ => Ok(()),
    }
  }
}

fn valid_path(value: &str) -> bool {
  !value.is_empty() && value.len() <= MAX_PATH_BYTES && !value.contains('\0')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
  File,
  Directory,
  Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
  pub name: String,
  pub path: String,
  pub entry_type: EntryType,
  pub size: u64,
  pub is_symlink: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitEntry {
  pub path: String,
  pub original_path: Option<String>,
  pub index_status: String,
  pub worktree_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
  Metadata {
    path: String,
    entry: Option<FileEntry>,
  },
  File {
    path: String,
    data_base64: String,
    offset: u64,
    total_size: u64,
    eof: bool,
  },
  Directory {
    path: String,
    entries: Vec<FileEntry>,
    next_offset: Option<u32>,
  },
  GitStatus {
    path: String,
    entries: Vec<GitEntry>,
  },
  Error {
    code: String,
    message: String,
  },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
  protocol_version: ProtocolVersion,
}

/// Serves the fixed negotiation before reading one bounded request.
///
/// # Errors
/// Rejects unavailable contracts and malformed requests.
pub async fn accept<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> io::Result<Request> {
  stream.write_all(PREFACE).await?;
  let offer = ProtocolOffer::new(
    PROTOCOL_BUILD,
    PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
  );
  write_frame(stream, &offer, 8192).await?;
  let selected: Selection = read_frame(stream, 8192).await?;
  if !offer.accepts(selected.protocol_version) {
    return Err(invalid("No shared published file service contract"));
  }
  let request: Request = read_frame(stream, MAX_REQUEST_BYTES).await?;
  request.validate()?;
  Ok(request)
}

/// Negotiates and performs exactly one request, retaining the write side until completion.
/// Dropping this future closes an owned stream and cancels its server operation.
///
/// # Errors
/// Returns negotiation, framing, timeout, or transport errors.
pub async fn request<S: AsyncRead + AsyncWrite + Unpin>(
  mut stream: S,
  request: &Request,
) -> io::Result<Response> {
  request.validate()?;
  tokio::time::timeout(STARTUP_TIMEOUT + OPERATION_TIMEOUT, async {
    let mut marker = [0; PREFACE.len()];
    stream.read_exact(&mut marker).await?;
    if marker != PREFACE {
      return Err(invalid("Invalid file service marker"));
    }
    let offer: ProtocolOffer = read_frame(&mut stream, 8192).await?;
    let protocol_version = offer
      .negotiate(SUPPORTED_PROTOCOL_VERSIONS)
      .ok_or_else(|| invalid("No shared published file service contract"))?;
    write_frame(&mut stream, &Selection { protocol_version }, 8192).await?;
    write_frame(&mut stream, request, MAX_REQUEST_BYTES).await?;
    read_frame(&mut stream, MAX_FRAME_BYTES).await
  })
  .await
  .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "File request timed out"))?
}

/// Writes one bounded result; service streams are never reused after completion.
///
/// # Errors
/// Rejects oversized responses or a closed transport.
pub async fn respond<S: AsyncWrite + Unpin>(stream: &mut S, response: &Response) -> io::Result<()> {
  write_frame(stream, response, MAX_FRAME_BYTES).await
}

async fn read_frame<T: DeserializeOwned>(
  reader: &mut (impl AsyncRead + Unpin),
  limit: usize,
) -> io::Result<T> {
  let size = reader.read_u32().await? as usize;
  if size == 0 || size > limit {
    return Err(invalid("Invalid file service frame size"));
  }
  let mut bytes = vec![0; size];
  reader.read_exact(&mut bytes).await?;
  serde_json::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

async fn write_frame<T: Serialize>(
  writer: &mut (impl AsyncWrite + Unpin),
  value: &T,
  limit: usize,
) -> io::Result<()> {
  let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
  if bytes.len() > limit {
    return Err(invalid("File service response exceeds its byte limit"));
  }
  writer
    .write_u32(u32::try_from(bytes.len()).map_err(io::Error::other)?)
    .await?;
  writer.write_all(&bytes).await?;
  writer.flush().await
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn validates_before_allocating_or_opening_paths() {
    assert!(
      Request::ReadFile {
        path: "file".into(),
        cwd: None,
        offset: 0,
        length: MAX_READ_BYTES + 1
      }
      .validate()
      .is_err()
    );
    assert!(
      Request::ListDirectory {
        path: "directory".into(),
        cwd: None,
        offset: 0,
        limit: 0
      }
      .validate()
      .is_err()
    );
    assert!(
      Request::Metadata {
        path: "invalid\0file".into(),
        cwd: None
      }
      .validate()
      .is_err()
    );
  }

  #[tokio::test]
  async fn oversized_frame_is_rejected_before_payload_allocation() {
    let (mut sender, mut receiver) = tokio::io::duplex(16);
    sender.write_u32(u32::MAX).await.unwrap();
    assert!(
      read_frame::<Response>(&mut receiver, MAX_FRAME_BYTES)
        .await
        .is_err()
    );
  }

  #[tokio::test]
  async fn unadvertised_contract_never_reads_a_file_request() {
    let (mut client, mut server) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move { accept(&mut server).await });
    let mut marker = [0; PREFACE.len()];
    client.read_exact(&mut marker).await.unwrap();
    let _: ProtocolOffer = read_frame(&mut client, 8192).await.unwrap();
    write_frame(
      &mut client,
      &Selection {
        protocol_version: ProtocolVersion::new(2, 0, 1),
      },
      8192,
    )
    .await
    .unwrap();
    assert!(server.await.unwrap().is_err());
  }
}
