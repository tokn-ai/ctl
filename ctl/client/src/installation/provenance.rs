use ctl_core::installations::CatalogOrigin;
use std::io;
use std::path::Path;

const MAX_ORIGIN_BYTES: usize = 512;
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Activates an uploaded native target without discarding channel components
/// that the single uploaded archive cannot replace.
/// Uploads without provenance reuse a known origin or register an explicit
/// provided source.
///
/// # Errors
/// Rejects a mixed target cohort, unsafe storage or concurrent channel changes.
pub fn activate_uploaded(
  home: &Path,
  bundle: &ctl_core::bundles::Bundle,
  origin: Option<&CatalogOrigin>,
) -> io::Result<(ctl_core::bundles::Bundle, ctl_core::installations::Lease)> {
  let installations = ctl_core::installations::Installations::new(home);
  let resolved_origin = if let Some(origin) = origin {
    origin.clone()
  } else {
    installations
      .catalog()?
      .into_iter()
      .filter(|entry| entry.bundle_id == bundle.manifest.bundle_id && entry.origin.is_some())
      .min_by_key(|entry| !entry.current)
      .and_then(|entry| entry.origin)
      .unwrap_or_else(|| CatalogOrigin {
        channel: "provided".into(),
        publication_id: Some(
          bundle
            .manifest
            .distribution_id
            .clone()
            .unwrap_or_else(|| bundle.manifest.bundle_id.clone()),
        ),
        built_at: None,
      })
  };
  let heads = single_target_heads(home, bundle, &resolved_origin, None)?;
  installations.activate_and_lease_with_origin_if_unchanged(bundle, &resolved_origin, &heads)
}

pub(crate) fn single_target_heads(
  home: &Path,
  bundle: &ctl_core::bundles::Bundle,
  origin: &CatalogOrigin,
  snapshot: Option<&[ctl_core::installations::CatalogEntry]>,
) -> io::Result<Vec<ctl_core::installations::CatalogEntry>> {
  origin.validate()?;
  let installations = ctl_core::installations::Installations::new(home);
  let heads = installations
    .catalog()?
    .into_iter()
    .filter(|entry| {
      entry.current
        && entry
          .origin
          .as_ref()
          .is_some_and(|existing| existing.channel == origin.channel)
    })
    .collect::<Vec<_>>();
  if let Some(snapshot) = snapshot {
    let expected = snapshot
      .iter()
      .filter(|entry| {
        entry.current
          && entry
            .origin
            .as_ref()
            .is_some_and(|existing| existing.channel == origin.channel)
      })
      .cloned()
      .collect::<Vec<_>>();
    if expected != heads {
      return Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "the channel's installed targets changed while preparing the build; retry the update",
      ));
    }
  }
  for head in &heads {
    if head.target_triple == bundle.manifest.target_triple {
      continue;
    }
    if head
      .origin
      .as_ref()
      .and_then(|origin| origin.publication_id.as_ref())
      != origin.publication_id.as_ref()
      || installations.get(&head.bundle_id)?.manifest.components["ctl-agent"].build
        != bundle.manifest.components["ctl-agent"].build
    {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "the single-target upload cannot replace this channel's other installed targets; run ctlup update on the target host to prepare its full target set, or explicitly remove the extra component registrations",
      ));
    }
  }
  Ok(heads)
}

/// Encodes validated installation provenance as bounded shell-safe hex.
///
/// # Errors
/// Rejects invalid or oversized provenance before constructing remote commands.
pub fn encode_catalog_origin(origin: &CatalogOrigin) -> io::Result<String> {
  origin.validate()?;
  let bytes = serde_json::to_vec(origin).map_err(io::Error::other)?;
  if bytes.len() > MAX_ORIGIN_BYTES {
    return Err(invalid());
  }
  let mut encoded = String::with_capacity(bytes.len() * 2);
  for byte in bytes {
    encoded.push(char::from(HEX[usize::from(byte >> 4)]));
    encoded.push(char::from(HEX[usize::from(byte & 15)]));
  }
  Ok(encoded)
}

/// Decodes bounded hex provenance and validates it before installation.
///
/// # Errors
/// Rejects malformed hex, unknown JSON fields and invalid origin metadata.
pub fn decode_catalog_origin(encoded: &str) -> io::Result<CatalogOrigin> {
  if encoded.is_empty() || encoded.len() > MAX_ORIGIN_BYTES * 2 || !encoded.len().is_multiple_of(2)
  {
    return Err(invalid());
  }
  let nibble = |byte: u8| match byte {
    b'0'..=b'9' => Ok(byte - b'0'),
    b'a'..=b'f' => Ok(byte - b'a' + 10),
    b'A'..=b'F' => Ok(byte - b'A' + 10),
    _ => Err(invalid()),
  };
  let bytes = encoded
    .as_bytes()
    .as_chunks::<2>()
    .0
    .iter()
    .map(|pair| Ok((nibble(pair[0])? << 4) | nibble(pair[1])?))
    .collect::<io::Result<Vec<_>>>()?;
  let origin: CatalogOrigin = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  origin.validate()?;
  Ok(origin)
}

fn invalid() -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidData,
    "invalid encoded installation provenance",
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(unix)]
  struct Home(std::path::PathBuf);
  #[cfg(unix)]
  impl Home {
    fn new() -> Self {
      let home =
        Self(std::env::temp_dir().join(format!("ctlup-provenance-{}", uuid::Uuid::new_v4())));
      ctl_core::private_fs::create_directory(&home.0).unwrap();
      home
    }
  }
  #[cfg(unix)]
  impl Drop for Home {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  #[cfg(unix)]
  fn fixture_bundle(home: &Path, target: &str, revision: char) -> ctl_core::bundles::Bundle {
    use crate::remote_bundle::compatibility::tests::Fixture;
    use ctl_core::bundles::{Manifest, Source, Store};
    let fixture = Fixture::new("0.1.3", &revision.to_string().repeat(40)).with_clients();
    let components = serde_json::from_value(fixture.inner["components"].clone()).unwrap();
    let manifest = Manifest::new(target, Source::Ci, components, &fixture.files).unwrap();
    Store::cache(home)
      .publish(&manifest, &fixture.files)
      .unwrap()
  }

  #[test]
  fn provenance_round_trips_as_shell_safe_hex() {
    let origin = CatalogOrigin {
      channel: "nightly".into(),
      publication_id: Some("0.1.3-nightly.20260102+gaaaaaaaaaaaa-123-1".into()),
      built_at: Some("2026-01-02T03:04:05Z".into()),
    };
    let encoded = encode_catalog_origin(&origin).unwrap();
    assert!(encoded.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(decode_catalog_origin(&encoded).unwrap(), origin);
    assert_eq!(
      decode_catalog_origin(&encoded.to_uppercase()).unwrap(),
      origin
    );
  }

  #[test]
  fn malformed_and_oversized_provenance_is_rejected() {
    for encoded in ["", "0", "gg", "00", &"00".repeat(MAX_ORIGIN_BYTES + 1)] {
      assert!(decode_catalog_origin(encoded).is_err());
    }
    let origin = CatalogOrigin {
      channel: "../stable".into(),
      publication_id: None,
      built_at: None,
    };
    assert!(encode_catalog_origin(&origin).is_err());
    let json = br#"{"channel":"stable","publication_id":null,"built_at":null,"extra":true}"#;
    let encoded: String = json
      .iter()
      .flat_map(|byte| {
        [
          char::from(HEX[usize::from(byte >> 4)]),
          char::from(HEX[usize::from(byte & 15)]),
        ]
      })
      .collect();
    assert!(decode_catalog_origin(&encoded).is_err());
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn archive_installation_records_origin_before_retention_cleanup() {
    use ctl_core::installations::Installations;
    let home = Home::new();
    let target = ctl_core::paths::native_target();
    let bundle = fixture_bundle(&home.0, target, 'a');
    // The receiver accepts native builds independently of publisher upload targets.
    let upload = crate::components::package_bundle(&bundle).unwrap();
    let origin = CatalogOrigin {
      channel: "nightly".into(),
      publication_id: Some("one".into()),
      built_at: None,
    };
    let installed = crate::installation::install_archive_with_origin(
      &home.0,
      &upload.archive,
      Some(origin.clone()),
      |_| {},
    )
    .await
    .unwrap();
    let store = Installations::new(&home.0);
    assert_eq!(
      store
        .component("nightly", target)
        .unwrap()
        .unwrap()
        .manifest,
      installed.manifest
    );
    assert_eq!(store.catalog().unwrap()[0].origin.as_ref(), Some(&origin));
  }

  #[cfg(unix)]
  #[test]
  fn single_target_upload_preserves_registered_siblings_and_rejects_partial_replacement() {
    use ctl_core::installations::Installations;
    let home = Home::new();
    let target = ctl_core::paths::native_target();
    let native = fixture_bundle(&home.0, target, 'a');
    let foreign_target = if target == "aarch64-unknown-linux-musl" {
      "x86_64-unknown-linux-musl"
    } else {
      "aarch64-unknown-linux-musl"
    };
    let foreign = fixture_bundle(&home.0, foreign_target, 'a');
    let store = Installations::new(&home.0);
    let origin = CatalogOrigin {
      channel: "nightly".into(),
      publication_id: Some("one".into()),
      built_at: None,
    };
    let installed = store
      .install_channel(&[native, foreign], &origin, true)
      .unwrap();
    let before = store.catalog().unwrap();
    let (_, lease) = activate_uploaded(&home.0, &installed[0], None).unwrap();
    assert_eq!(store.catalog().unwrap(), before);
    let fresh = fixture_bundle(&home.0, target, 'b');
    let fresh_origin = CatalogOrigin {
      publication_id: Some("two".into()),
      ..origin
    };
    assert!(
      activate_uploaded(&home.0, &fresh, Some(&fresh_origin))
        .unwrap_err()
        .to_string()
        .contains("run ctlup update on the target host")
    );
    assert_eq!(store.catalog().unwrap(), before);
    assert_eq!(
      store.active().unwrap().unwrap().manifest,
      installed[0].manifest
    );
    drop(lease);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn originless_archives_always_record_explicit_provided_identity() {
    use ctl_core::bundles::Store;
    use ctl_core::installations::Installations;
    let home = Home::new();
    let target = ctl_core::paths::native_target();
    let original = fixture_bundle(&home.0, target, 'a');
    let store = Installations::new(&home.0);
    let (_, lease) = activate_uploaded(&home.0, &original, None).unwrap();
    assert!(store.catalog().unwrap().iter().all(|entry| {
      entry
        .origin
        .as_ref()
        .is_some_and(|origin| origin.channel == "provided" && origin.built_at.is_none())
    }));
    drop(lease);

    let next = fixture_bundle(&home.0, target, 'b');
    let manifest = next
      .manifest
      .clone()
      .with_distribution_id("published-fixture")
      .unwrap();
    let next = Store::cache(&home.0)
      .publish(&manifest, &next.read_files().unwrap())
      .unwrap();
    let upload = crate::components::package_bundle(&next).unwrap();
    let installed = crate::installation::install_archive(&home.0, &upload.archive)
      .await
      .unwrap();
    let registered = store.component("provided", target).unwrap().unwrap();
    assert_eq!(registered.manifest, installed.manifest);
    let entry = store
      .catalog()
      .unwrap()
      .into_iter()
      .find(|entry| entry.current && entry.bundle_id == installed.manifest.bundle_id)
      .unwrap();
    assert_eq!(
      entry.origin,
      Some(CatalogOrigin {
        channel: "provided".into(),
        publication_id: Some("published-fixture".into()),
        built_at: None,
      })
    );
    let without_distribution = fixture_bundle(&home.0, target, 'c');
    let (installed, lease) = activate_uploaded(&home.0, &without_distribution, None).unwrap();
    assert!(store.catalog().unwrap().iter().any(|entry| {
      entry.current
        && entry.bundle_id == installed.manifest.bundle_id
        && entry.origin.as_ref().is_some_and(|origin| {
          origin.channel == "provided"
            && origin.publication_id.as_deref() == Some(installed.manifest.bundle_id.as_str())
        })
    }));
    drop(lease);
  }

  #[cfg(unix)]
  #[test]
  fn originless_upload_does_not_replace_provided_foreign_siblings() {
    use ctl_core::installations::Installations;
    let home = Home::new();
    let target = ctl_core::paths::native_target();
    let native = fixture_bundle(&home.0, target, 'a');
    let foreign_target = if target == "aarch64-unknown-linux-musl" {
      "x86_64-unknown-linux-musl"
    } else {
      "aarch64-unknown-linux-musl"
    };
    let foreign = fixture_bundle(&home.0, foreign_target, 'a');
    let store = Installations::new(&home.0);
    let origin = CatalogOrigin {
      channel: "provided".into(),
      publication_id: Some("one".into()),
      built_at: None,
    };
    let installed = store
      .install_channel(&[native, foreign], &origin, true)
      .unwrap();
    let before = store.catalog().unwrap();
    let fresh = fixture_bundle(&home.0, target, 'b');
    let error = activate_uploaded(&home.0, &fresh, None).unwrap_err();
    assert!(
      error
        .to_string()
        .contains("run ctlup update on the target host")
    );
    assert_eq!(store.catalog().unwrap(), before);
    assert_eq!(
      store.active().unwrap().unwrap().manifest,
      installed[0].manifest
    );
  }
}
