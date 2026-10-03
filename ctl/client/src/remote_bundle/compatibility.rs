//! Protocol eligibility is separate from each immutable bundle's provenance.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;

use ctl_core::component::{ComponentInfo, protocols_match};
use ctl_core::protocol::ProtocolVersion;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::{BundleSet, Error, MAX_BUNDLE_SET_BYTES};

pub(super) const COMPONENTS: [&str; 3] = ["ctl-agent", "ctmuxd", "ctl-taskd"];
const MAX_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 3 * MAX_BINARY_BYTES + MAX_BUNDLE_SET_BYTES as u64 + 1024 * 1024;

pub(super) fn deserialize_components<'de, D: serde::Deserializer<'de>>(
  deserializer: D,
) -> Result<Option<BTreeMap<String, ComponentInfo>>, D::Error> {
  struct ComponentsVisitor;
  impl<'de> serde::de::Visitor<'de> for ComponentsVisitor {
    type Value = BTreeMap<String, ComponentInfo>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      formatter.write_str("a uniquely named map of three component advertisements")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
      let mut components = BTreeMap::new();
      while let Some((name, component)) = map.next_entry::<String, ComponentInfo>()? {
        if components.insert(name, component).is_some() || components.len() > COMPONENTS.len() {
          return Err(serde::de::Error::custom(
            "duplicate or excessive bundle components",
          ));
        }
      }
      Ok(components)
    }
  }
  deserializer.deserialize_map(ComponentsVisitor).map(Some)
}

pub(super) fn validate_components(
  manifest: &BundleSet,
  components: Option<&BTreeMap<String, ComponentInfo>>,
) -> Result<(), Error> {
  if manifest.schema_version == 1 {
    return if components.is_none() {
      Ok(())
    } else {
      Err(invalid(
        "schema-1 bundles cannot declare protocol compatibility",
      ))
    };
  }
  let components = components.ok_or_else(|| invalid("schema-2 bundle lacks component metadata"))?;
  if components.len() != COMPONENTS.len()
    || COMPONENTS
      .iter()
      .any(|name| !components.contains_key(*name))
  {
    return Err(invalid(
      "bundle metadata must identify exactly the three shipped components",
    ));
  }
  for (name, info) in components {
    if !info.is_valid()
      || info.build.dirty
      || info.build.version != manifest.app_version
      || info.build.source_revision.as_deref() != Some(&manifest.git_revision)
    {
      return Err(invalid(
        "component provenance does not match the clean bundle identity",
      ));
    }
    let required: &[&str] = match name.as_str() {
      "ctl-agent" => &[
        "ctl_identity",
        "ctl_maintenance",
        "ctmux",
        "ctmux_control",
        "task",
        "task_control",
      ],
      "ctmuxd" => &["ctmux", "ctmux_control"],
      "ctl-taskd" => &["task", "task_control", "ctmux", "ctmux_control"],
      _ => unreachable!("component names were checked above"),
    };
    if required
      .iter()
      .any(|name| !info.protocols.iter().any(|protocol| protocol.name == *name))
    {
      return Err(invalid(
        "component metadata omits a required service or companion protocol",
      ));
    }
  }
  Ok(())
}

pub(super) fn compatible(components: &BTreeMap<String, ComponentInfo>) -> bool {
  let required = [
    (
      "ctl-agent",
      "ctl_identity",
      ctl_proto::IDENTITY_SUPPORTED_PROTOCOL_VERSIONS,
    ),
    (
      "ctl-agent",
      "ctl_maintenance",
      ctl_proto::maintenance::SUPPORTED_PROTOCOL_VERSIONS,
    ),
    ("ctmuxd", "ctmux", ctmux_proto::SUPPORTED_PROTOCOL_VERSIONS),
    (
      "ctmuxd",
      "ctmux_control",
      ctmux_ipc::LOCAL_CONTROL_SUPPORTED_PROTOCOL_VERSIONS,
    ),
    (
      "ctl-taskd",
      "task",
      ctl_task_proto::SUPPORTED_PROTOCOL_VERSIONS,
    ),
    (
      "ctl-taskd",
      "task_control",
      ctl_task_proto::control::SUPPORTED_PROTOCOL_VERSIONS,
    ),
  ];
  required
    .iter()
    .all(|(component, name, supported)| intersects(components, component, name, supported))
    && [
      ("ctl-agent", "ctmuxd", "ctmux"),
      ("ctl-agent", "ctmuxd", "ctmux_control"),
      ("ctl-agent", "ctl-taskd", "task"),
      ("ctl-agent", "ctl-taskd", "task_control"),
      ("ctl-taskd", "ctmuxd", "ctmux"),
      ("ctl-taskd", "ctmuxd", "ctmux_control"),
    ]
    .iter()
    .all(|(consumer, provider, name)| {
      components
        .get(*consumer)
        .and_then(|info| {
          info
            .protocols
            .iter()
            .find(|protocol| protocol.name == *name)
        })
        .is_some_and(|consumer| {
          intersects(components, provider, name, &consumer.supported_versions)
        })
    })
}

fn intersects(
  components: &BTreeMap<String, ComponentInfo>,
  component: &str,
  name: &str,
  supported: &[ProtocolVersion],
) -> bool {
  components
    .get(component)
    .and_then(|info| info.protocols.iter().find(|protocol| protocol.name == name))
    .is_some_and(|protocol| protocol.negotiate(supported).is_some())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveManifest {
  schema_version: u32,
  app_version: String,
  bundle_id: String,
  git_revision: String,
  target_triple: String,
  files: BTreeMap<String, String>,
  #[serde(deserialize_with = "deserialize_components")]
  components: Option<BTreeMap<String, ComponentInfo>>,
}

/// Parse flat regular tar entries without writing or executing foreign binaries.
pub(super) fn verify_archive(
  manifest: &BundleSet,
  target: &str,
  bytes: &[u8],
) -> Result<(), Error> {
  let decoder = flate2::read::MultiGzDecoder::new(bytes).take(MAX_EXPANDED_BYTES + 1);
  let mut archive = tar::Archive::new(decoder);
  let mut hashes = BTreeMap::new();
  let mut names = BTreeSet::new();
  let mut inner = None;
  let mut payload_end = 0;
  for entry in archive.entries().map_err(archive_error)?.raw(true) {
    let mut entry = entry.map_err(archive_error)?;
    let name = entry.path_bytes();
    let name = std::str::from_utf8(&name)
      .map_err(|_| invalid("non-UTF8 archive path"))?
      .to_owned();
    if entry.header().entry_type() != tar::EntryType::Regular
      || (!COMPONENTS.contains(&name.as_str()) && name != "manifest.json")
      || !names.insert(name.clone())
    {
      return Err(invalid(
        "bundle archive contains links, special, duplicate, or unexpected paths",
      ));
    }
    if entry.size() == 0 || entry.size() > MAX_BINARY_BYTES {
      return Err(invalid(
        "bundle entry is empty or exceeds its expanded size limit",
      ));
    }
    payload_end += 512 + entry.size().div_ceil(512) * 512;
    if name == "manifest.json" {
      let mut bytes = Vec::new();
      entry
        .take(MAX_BUNDLE_SET_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(archive_error)?;
      if bytes.len() > MAX_BUNDLE_SET_BYTES {
        return Err(invalid("archive component manifest exceeds its size limit"));
      }
      inner = Some(
        serde_json::from_slice::<ArchiveManifest>(&bytes)
          .map_err(|_| invalid("invalid archive component manifest"))?,
      );
    } else {
      if entry.header().mode().map_err(archive_error)? & 0o111 == 0 {
        return Err(invalid("bundle binary is not executable"));
      }
      let mut digest = Sha256::new();
      let mut buffer = vec![0; 64 * 1024];
      loop {
        let length = entry.read(&mut buffer).map_err(archive_error)?;
        if length == 0 {
          break;
        }
        digest.update(&buffer[..length]);
      }
      hashes.insert(name, format!("{:x}", digest.finalize()));
    }
  }
  // Drain the gzip trailer to validate its CRC, reject hidden trailing entries,
  // and enforce a total decompressed bound including tar padding/headers.
  validate_padding(archive.into_inner(), payload_end)?;
  let inner = inner.ok_or_else(|| invalid("bundle archive lacks its component manifest"))?;
  if inner.schema_version != 2
    || inner.app_version != manifest.app_version
    || inner.bundle_id != manifest.bundle_id
    || inner.git_revision != manifest.git_revision
    || inner.target_triple != target
    || hashes.len() != COMPONENTS.len()
    || inner.files.len() != COMPONENTS.len()
    || hashes.iter().any(|(name, hash)| {
      inner
        .files
        .get(name)
        .is_none_or(|expected| !expected.eq_ignore_ascii_case(hash))
    })
  {
    return Err(invalid(
      "archive manifest identity or actual binary hashes disagree with the bundle",
    ));
  }
  validate_components(manifest, inner.components.as_ref())?;
  let outer = manifest
    .target(target)?
    .components
    .as_ref()
    .ok_or_else(|| invalid("missing outer component metadata"))?;
  if inner.components.as_ref().is_none_or(|inner| {
    outer.iter().any(|(name, expected)| {
      inner.get(name).is_none_or(|actual| {
        expected.build != actual.build || !protocols_match(&expected.protocols, &actual.protocols)
      })
    })
  }) {
    return Err(invalid(
      "archive component advertisements disagree with the bundle set",
    ));
  }
  Ok(())
}

fn validate_padding<R: std::io::Read>(
  mut reader: std::io::Take<R>,
  payload_end: u64,
) -> Result<(), Error> {
  let mut buffer = vec![0; 64 * 1024];
  loop {
    let length = reader.read(&mut buffer).map_err(archive_error)?;
    if length == 0 {
      break;
    }
    if buffer[..length].iter().any(|byte| *byte != 0) {
      return Err(invalid("bundle archive contains trailing non-padding data"));
    }
  }
  let expanded = MAX_EXPANDED_BYTES + 1 - reader.limit();
  if reader.limit() == 0 {
    return Err(invalid("bundle archive exceeds its expanded size limit"));
  }
  if expanded < payload_end + 1024 || !expanded.is_multiple_of(512) {
    return Err(invalid("bundle archive lacks complete tar end blocks"));
  }
  Ok(())
}

fn archive_error(error: impl std::fmt::Display) -> Error {
  invalid(&format!("invalid compressed bundle archive: {error}"))
}

fn invalid(message: &str) -> Error {
  Error::Invalid(message.into())
}

#[cfg(test)]
pub(super) mod tests;
