//! Compile-time metadata for a development packager that cannot run the target.
//! Keep the record encoding in sync with scripts/shared/elf-component.mts.

use super::ProtocolContract;
use crate::protocol::ProtocolVersion;

const MAGIC: &[u8] = b"\0tokn-ctl-component\0\x01";
const TARGET: &str = env!("COMPONENT_TARGET");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const REVISION: &str = env!("COMPONENT_SOURCE_REVISION");
const FINGERPRINT: &str = env!("COMPONENT_SOURCE_FINGERPRINT");
const DIRTY: &str = env!("COMPONENT_SOURCE_DIRTY");

#[must_use]
pub const fn record_len(name: &str, contracts: &[ProtocolContract]) -> usize {
  // Four bytes for the payload size, two per string length, one for the dirty
  // flag and two for the number of contracts.
  let mut len = MAGIC.len()
    + 4
    + 5 * 2
    + 1
    + 2
    + name.len()
    + TARGET.len()
    + VERSION.len()
    + REVISION.len()
    + FINGERPRINT.len();
  let mut index = 0;
  while index < contracts.len() {
    let contract = contracts[index];
    len += 2 + contract.name.len() + 2 + 6 + 2 + 6 * contract.supported.len();
    index += 1;
  }
  len
}

struct Record<const N: usize> {
  bytes: [u8; N],
  offset: usize,
}

impl<const N: usize> Record<N> {
  const fn bytes(&mut self, bytes: &[u8]) {
    let mut index = 0;
    while index < bytes.len() {
      self.bytes[self.offset] = bytes[index];
      self.offset += 1;
      index += 1;
    }
  }

  const fn word(&mut self, word: u16) {
    self.bytes(&word.to_le_bytes());
  }

  const fn text(&mut self, text: &str) {
    assert!(text.len() <= 256);
    #[expect(
      clippy::cast_possible_truncation,
      reason = "text length is bounded above"
    )]
    self.word(text.len() as u16);
    self.bytes(text.as_bytes());
  }

  const fn version(&mut self, version: ProtocolVersion) {
    self.word(version.major);
    self.word(version.minor);
    self.word(version.build);
  }

  const fn contract(&mut self, contract: ProtocolContract) {
    self.text(contract.name);
    self.word(contract.build);
    self.version(contract.version);
    assert!(contract.supported.len() <= 128);
    #[expect(
      clippy::cast_possible_truncation,
      reason = "contract versions are bounded above"
    )]
    self.word(contract.supported.len() as u16);
    let mut index = 0;
    while index < contract.supported.len() {
      self.version(contract.supported[index]);
      index += 1;
    }
  }
}

/// Encodes the current target, component identity and protocol advertisements.
///
/// # Panics
/// Panics if the record would exceed the metadata bounds or its declared length.
#[must_use]
pub const fn record<const N: usize>(name: &str, contracts: &[ProtocolContract]) -> [u8; N] {
  assert!(N <= 16 * 1024 && contracts.len() <= 128);
  let mut record = Record {
    bytes: [0; N],
    offset: 0,
  };
  record.bytes(MAGIC);
  #[expect(
    clippy::cast_possible_truncation,
    reason = "metadata record is bounded above"
  )]
  record.bytes(&((N - MAGIC.len() - 4) as u32).to_le_bytes());
  for_record(&mut record, name, contracts);
  assert!(record.offset == N);
  record.bytes
}

const fn for_record<const N: usize>(
  record: &mut Record<N>,
  name: &str,
  contracts: &[ProtocolContract],
) {
  record.text(name);
  record.text(TARGET);
  record.text(VERSION);
  record.text(REVISION);
  record.text(FINGERPRINT);
  record.bytes(&[if DIRTY.as_bytes()[0] == b't' { 1 } else { 0 }]);
  #[expect(
    clippy::cast_possible_truncation,
    reason = "the caller bounds the contract count"
  )]
  record.word(contracts.len() as u16);
  let mut index = 0;
  while index < contracts.len() {
    record.contract(contracts[index]);
    index += 1;
  }
}

/// Retains a cross-build record in the executable's own compilation unit.
#[macro_export]
macro_rules! retain_component_metadata {
  ($name:literal, $contracts:expr) => {{
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    {
      use $crate::component::embedded::{record, record_len};
      const CONTRACTS: &[$crate::component::ProtocolContract] = $contracts;
      static RECORD: [u8; record_len($name, CONTRACTS)] = record($name, CONTRACTS);
      // Keep a reference to the full byte record through compiler and linker GC.
      std::hint::black_box(&RECORD);
    }
  }};
}

/// Retains the linked record and reports metadata from the same contracts.
#[macro_export]
macro_rules! embedded_component_info {
  ($name:literal, $contracts:expr) => {{
    $crate::retain_component_metadata!($name, $contracts);
    $crate::component::ComponentInfo::from_contracts($contracts)
  }};
}
