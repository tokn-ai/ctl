//! Concrete published protocol contracts, independent of product releases.
//!
//! A build number identifies a published wire contract. Internal builds may be
//! skipped, so compatibility is an explicit advertised set rather than a range.

use std::fmt;
use std::str::FromStr;

use serde::ser::SerializeStruct as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const MAX_SUPPORTED_VERSIONS: usize = 128;

/// One canonical `major.minor.build` contract. Ordering follows those fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion {
  pub major: u16,
  pub minor: u16,
  pub build: u16,
}

impl ProtocolVersion {
  /// Constructs a triplet; wire validation rejects a zero major version.
  #[must_use]
  pub const fn new(major: u16, minor: u16, build: u16) -> Self {
    Self {
      major,
      minor,
      build,
    }
  }

  #[must_use]
  pub const fn is_valid(self) -> bool {
    self.major != 0
  }
}

impl fmt::Display for ProtocolVersion {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "{}.{}.{}", self.major, self.minor, self.build)
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseProtocolVersionError;

impl fmt::Display for ParseProtocolVersionError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(
      "protocol version must be a canonical major.minor.build triplet with a nonzero major",
    )
  }
}

impl std::error::Error for ParseProtocolVersionError {}

impl FromStr for ProtocolVersion {
  type Err = ParseProtocolVersionError;

  fn from_str(value: &str) -> Result<Self, Self::Err> {
    if value.len() > 17 {
      return Err(ParseProtocolVersionError);
    }
    let mut parts = value.split('.');
    let mut part = || {
      let value = parts.next().ok_or(ParseProtocolVersionError)?;
      if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
      {
        return Err(ParseProtocolVersionError);
      }
      value.parse::<u16>().map_err(|_| ParseProtocolVersionError)
    };
    let version = Self::new(part()?, part()?, part()?);
    if parts.next().is_some() || !version.is_valid() {
      return Err(ParseProtocolVersionError);
    }
    Ok(version)
  }
}

impl Serialize for ProtocolVersion {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    if !self.is_valid() {
      return Err(serde::ser::Error::custom(ParseProtocolVersionError));
    }
    serializer.collect_str(self)
  }
}

impl<'de> Deserialize<'de> for ProtocolVersion {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    struct VersionVisitor;

    impl serde::de::Visitor<'_> for VersionVisitor {
      type Value = ProtocolVersion;

      fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a canonical major.minor.build protocol version string")
      }

      fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        value.parse().map_err(E::custom)
      }
    }

    deserializer.deserialize_str(VersionVisitor)
  }
}

/// A component's latest published contract and the contracts it implements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolOffer {
  pub build: u16,
  pub version: ProtocolVersion,
  pub supported_versions: Vec<ProtocolVersion>,
}

impl ProtocolOffer {
  #[must_use]
  pub fn new(build: u16, version: ProtocolVersion, supported: &[ProtocolVersion]) -> Self {
    Self {
      build,
      version,
      supported_versions: supported.to_vec(),
    }
  }

  /// Checks the bounded published set and its monotonic build identities.
  #[must_use]
  pub fn is_valid(&self) -> bool {
    valid_offer(self.build, self.version, &self.supported_versions)
  }

  /// Selects the newest contract explicitly implemented by both peers.
  /// Invalid advertisements or major mismatches have no common contract.
  #[must_use]
  pub fn negotiate(&self, local: &[ProtocolVersion]) -> Option<ProtocolVersion> {
    if !self.is_valid() {
      return None;
    }
    negotiate_versions(&self.supported_versions, local)
  }

  /// Accepts only a concrete advertised contract, never an inferred range.
  #[must_use]
  pub fn accepts(&self, selected: ProtocolVersion) -> bool {
    self.is_valid() && self.supported_versions.contains(&selected)
  }
}

pub(crate) fn negotiate_versions(
  supported: &[ProtocolVersion],
  local: &[ProtocolVersion],
) -> Option<ProtocolVersion> {
  if !valid_versions(local) {
    return None;
  }
  local
    .iter()
    .copied()
    .filter(|version| supported.contains(version))
    .max()
}

pub(crate) fn valid_offer(
  build: u16,
  version: ProtocolVersion,
  supported: &[ProtocolVersion],
) -> bool {
  version.is_valid()
    && build >= version.build
    && valid_versions(supported)
    && supported.contains(&version)
    && supported.iter().all(|candidate| {
      candidate.major == version.major && *candidate <= version && candidate.build <= build
    })
}

fn valid_versions(versions: &[ProtocolVersion]) -> bool {
  !versions.is_empty()
    && versions.len() <= MAX_SUPPORTED_VERSIONS
    && versions.iter().enumerate().all(|(index, version)| {
      version.is_valid()
        && version.major == versions[0].major
        && versions[..index].iter().all(|earlier| {
          version.cmp(earlier) == version.build.cmp(&earlier.build)
            && version.build != earlier.build
        })
    })
}

impl Serialize for ProtocolOffer {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    if !self.is_valid() {
      return Err(serde::ser::Error::custom(
        "invalid published protocol offer",
      ));
    }
    let mut fields = serializer.serialize_struct("ProtocolOffer", 3)?;
    fields.serialize_field("build", &self.build)?;
    fields.serialize_field("version", &self.version)?;
    fields.serialize_field("supported_versions", &self.supported_versions)?;
    fields.end()
  }
}

impl<'de> Deserialize<'de> for ProtocolOffer {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fields {
      build: u16,
      version: ProtocolVersion,
      supported_versions: Vec<ProtocolVersion>,
    }

    let fields = Fields::deserialize(deserializer)?;
    let offer = Self {
      build: fields.build,
      version: fields.version,
      supported_versions: fields.supported_versions,
    };
    if !offer.is_valid() {
      return Err(serde::de::Error::custom("invalid published protocol offer"));
    }
    Ok(offer)
  }
}

#[cfg(test)]
mod tests;
