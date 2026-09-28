//! Classifies container output without retaining logs or returning server data.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

const READ_BYTES: usize = 2048;
const DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

// More specific causes take precedence over secondary connection failures.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Failure {
  Connection,
  Dns,
  Authentication,
  Certificate,
  Configuration,
  MissingImage,
  EngineUnavailable,
}

impl Failure {
  const fn message(self) -> &'static str {
    match self {
      Self::Connection => {
        "The VPN gateway could not be reached. Check the server address, network connection, and any required port."
      }
      Self::Dns => {
        "The VPN server name could not be resolved. Check the server address and DNS connection."
      }
      Self::Authentication => {
        "VPN authentication did not complete. Check the username, password, and authentication method in the connection settings."
      }
      Self::Certificate => {
        "The VPN server certificate could not be verified. Check the server address and certificate trust configuration."
      }
      Self::Configuration => {
        "The VPN container could not load its configuration. Rebuild the image with docker/openconnect/run.sh build and try again."
      }
      Self::MissingImage => {
        "The OpenConnect container image is missing. Build it with docker/openconnect/run.sh build."
      }
      Self::EngineUnavailable => {
        "The container engine is unavailable. Start Docker or the Podman machine and try again."
      }
    }
  }
}

// Match fixed signatures only. OpenConnect and engine output can contain
// credentials, internal addresses, certificate details, and server-supplied text.
const SIGNATURES: &[(&[u8], Failure)] = &[
  (b"failed to connect to", Failure::Connection),
  (b"connection refused", Failure::Connection),
  (b"connection timed out", Failure::Connection),
  (b"no route to host", Failure::Connection),
  (b"network is unreachable", Failure::Connection),
  (b"getaddrinfo failed", Failure::Dns),
  (b"could not resolve host", Failure::Dns),
  (b"name or service not known", Failure::Dns),
  (b"temporary failure in name resolution", Failure::Dns),
  (b"nodename nor servname provided", Failure::Dns),
  (b"login failed", Failure::Authentication),
  (b"authentication failed", Failure::Authentication),
  (b"login denied", Failure::Authentication),
  (b"server certificate verify failed", Failure::Certificate),
  (b"certificate verification failed", Failure::Certificate),
  (b"certificate verify failed", Failure::Certificate),
  (
    b"vpn configuration was not received",
    Failure::Configuration,
  ),
  (b"vpn configuration is too large", Failure::Configuration),
  (b"vpn configuration is empty", Failure::Configuration),
  (b"vpn configuration is invalid", Failure::Configuration),
  (b"mount the vpn .env file", Failure::Configuration),
  (b"no such image", Failure::MissingImage),
  (b"image not known", Failure::MissingImage),
  (b"unable to find image", Failure::MissingImage),
  (
    b"cannot connect to the docker daemon",
    Failure::EngineUnavailable,
  ),
  (b"cannot connect to podman", Failure::EngineUnavailable),
  (b"is the docker daemon running", Failure::EngineUnavailable),
];

type SharedFailure = Arc<Mutex<Option<Failure>>>;

pub(super) struct Diagnostics {
  failure: SharedFailure,
  readers: Vec<JoinHandle<()>>,
}

impl Diagnostics {
  pub(super) fn new(
    stdout: impl AsyncRead + Unpin + Send + 'static,
    stderr: impl AsyncRead + Unpin + Send + 'static,
  ) -> Self {
    let failure = Arc::new(Mutex::new(None));
    let readers = vec![
      tokio::spawn(drain(stdout, failure.clone())),
      tokio::spawn(drain(stderr, failure.clone())),
    ];
    Self { failure, readers }
  }

  pub(super) async fn finish(&mut self) -> Option<&'static str> {
    // A grandchild could keep a pipe open after the engine exits. Never let
    // diagnostic collection delay cancellation or container cleanup indefinitely.
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, async {
      while let Some(reader) = self.readers.last_mut() {
        let _ = reader.await;
        self.readers.pop();
      }
    })
    .await;
    for reader in self.readers.drain(..) {
      reader.abort();
    }
    self
      .failure
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .map(Failure::message)
  }
}

impl Drop for Diagnostics {
  fn drop(&mut self) {
    for reader in &self.readers {
      reader.abort();
    }
  }
}

async fn drain(mut reader: impl AsyncRead + Unpin, failure: SharedFailure) {
  let mut classifier = Classifier::new();
  let mut bytes = Zeroizing::new([0_u8; READ_BYTES]);
  while let Ok(count) = reader.read(bytes.as_mut()).await {
    if count == 0 {
      break;
    }
    if let Some(found) = classifier.push(&bytes[..count]) {
      let mut current = failure
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      *current = (*current).max(Some(found));
    }
    bytes.fill(0);
  }
}

struct Classifier {
  patterns: Vec<Pattern>,
}

impl Classifier {
  fn new() -> Self {
    Self {
      patterns: SIGNATURES
        .iter()
        .map(|&(bytes, failure)| Pattern::new(bytes, failure))
        .collect(),
    }
  }

  fn push(&mut self, bytes: &[u8]) -> Option<Failure> {
    let mut found = None;
    for &byte in bytes {
      for pattern in &mut self.patterns {
        if pattern.push(byte.to_ascii_lowercase()) {
          found = found.max(Some(pattern.failure));
        }
      }
    }
    found
  }
}

// KMP progress stores positions in static patterns, never bytes from the log.
// Memory remains constant even for huge lines, invalid UTF-8, or split signatures.
struct Pattern {
  bytes: &'static [u8],
  prefixes: Vec<usize>,
  matched: usize,
  failure: Failure,
}

impl Pattern {
  fn new(bytes: &'static [u8], failure: Failure) -> Self {
    let mut prefixes = vec![0; bytes.len()];
    for index in 1..bytes.len() {
      let mut matched = prefixes[index - 1];
      while matched > 0 && bytes[index] != bytes[matched] {
        matched = prefixes[matched - 1];
      }
      if bytes[index] == bytes[matched] {
        matched += 1;
      }
      prefixes[index] = matched;
    }
    Self {
      bytes,
      prefixes,
      matched: 0,
      failure,
    }
  }

  fn push(&mut self, byte: u8) -> bool {
    while self.matched > 0 && byte != self.bytes[self.matched] {
      self.matched = self.prefixes[self.matched - 1];
    }
    if byte == self.bytes[self.matched] {
      self.matched += 1;
    }
    let found = self.matched == self.bytes.len();
    if found {
      self.matched = self.prefixes[self.matched - 1];
    }
    found
  }
}

#[cfg(test)]
mod tests {
  use tokio::io::AsyncWriteExt as _;

  use super::*;

  #[test]
  fn recognizes_split_signatures_without_retaining_input() {
    for &(signature, expected) in SIGNATURES {
      for split in 0..=signature.len() {
        let mut classifier = Classifier::new();
        let first = classifier.push(&signature[..split].to_ascii_uppercase());
        let second = classifier.push(&signature[split..]);
        assert!(first.max(second) == Some(expected));
      }
    }
  }

  #[test]
  fn very_long_invalid_lines_do_not_expand_classifier_memory() {
    let mut classifier = Classifier::new();
    let original_capacity: usize = classifier
      .patterns
      .iter()
      .map(|p| p.prefixes.capacity())
      .sum();
    for _ in 0..1024 {
      assert!(classifier.push(&[0xff; READ_BYTES]).is_none());
    }
    let capacity: usize = classifier
      .patterns
      .iter()
      .map(|p| p.prefixes.capacity())
      .sum();
    assert_eq!(capacity, original_capacity);
    assert!(classifier.push(b"Login failed").is_some());
  }

  #[tokio::test]
  async fn drains_both_streams_and_returns_only_static_safe_diagnostics() {
    let (mut stdout, output_reader) = tokio::io::duplex(32);
    let (mut stderr, error_reader) = tokio::io::duplex(32);
    let mut diagnostics = Diagnostics::new(output_reader, error_reader);
    let output = tokio::spawn(async move {
      for _ in 0..100 {
        stdout
          .write_all(b"private-test-password\xff")
          .await
          .unwrap();
      }
      stdout
        .write_all(b"Failed to connect to https://private.example.test\n")
        .await
        .unwrap();
    });
    let error = tokio::spawn(async move {
      stderr.write_all(b"user=private-user password=private-test-password\nServer certificate verify failed: private.example.test\n").await.unwrap();
    });
    output.await.unwrap();
    error.await.unwrap();
    let message = diagnostics.finish().await.unwrap();
    assert_eq!(message, Failure::Certificate.message());
    assert!(!message.contains("private"));
  }

  #[tokio::test]
  async fn unknown_output_is_not_returned_as_a_diagnostic() {
    let mut diagnostics = Diagnostics::new(
      b"unexpected private-test-password response".as_slice(),
      b"https://private.example.test/unknown".as_slice(),
    );
    assert!(diagnostics.finish().await.is_none());
  }

  #[tokio::test]
  async fn open_pipes_cannot_block_diagnostic_collection() {
    let (_stdout, output_reader) = tokio::io::duplex(32);
    let (_stderr, error_reader) = tokio::io::duplex(32);
    let mut diagnostics = Diagnostics::new(output_reader, error_reader);
    let readers: Vec<_> = diagnostics
      .readers
      .iter()
      .map(JoinHandle::abort_handle)
      .collect();
    assert!(
      tokio::time::timeout(Duration::from_secs(1), diagnostics.finish())
        .await
        .unwrap()
        .is_none()
    );
    tokio::task::yield_now().await;
    assert!(readers.iter().all(tokio::task::AbortHandle::is_finished));
    assert!(diagnostics.finish().await.is_none());
  }
}
