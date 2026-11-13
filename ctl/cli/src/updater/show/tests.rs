use super::*;
use ctl_client::installation::InstallationCatalog;
use ctl_core::bundle_layout::{ALL, required_protocols};
use ctl_core::bundles::Manifest;
use ctl_core::component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo};
use ctl_core::installations::CatalogOrigin;
use ctl_core::protocol::ProtocolVersion;

fn manifest(token: char, source: Source, dirty: bool) -> Manifest {
  let build = ComponentBuildInfo {
    version: "0.1.3".into(),
    source_revision: Some(token.to_string().repeat(40)),
    source_fingerprint: token.to_string().repeat(64),
    dirty,
  };
  let version = ProtocolVersion::new(1, 0, 1);
  let components = ALL
    .into_iter()
    .map(|name| {
      let protocols = required_protocols(name)
        .iter()
        .map(|&name| ProtocolInfo::new(name, 1, version, &[version]))
        .collect();
      (
        name.into(),
        ComponentInfo {
          build: build.clone(),
          protocols,
        },
      )
    })
    .collect();
  let files = ALL
    .into_iter()
    .map(|name| (name.into(), token.to_string().into_bytes()))
    .collect();
  Manifest::new("x86_64-unknown-linux-musl", source, components, &files).unwrap()
}

fn snapshot(manifests: &[Manifest]) -> InstallationShow {
  InstallationShow {
    schema_version: 1,
    active: None,
    previous: None,
    installations: manifests
      .iter()
      .map(|manifest| entry(manifest, "provided", false))
      .collect(),
    builds: manifests
      .iter()
      .map(|manifest| InstalledBuild {
        bundle_id: manifest.bundle_id.clone(),
        target_triple: manifest.target_triple.clone(),
        source: manifest.source,
        build: manifest.components["ctl-agent"].build.clone(),
      })
      .collect(),
  }
}

fn entry(manifest: &Manifest, channel: &str, current: bool) -> CatalogEntry {
  CatalogEntry {
    bundle_id: manifest.bundle_id.clone(),
    target_triple: manifest.target_triple.clone(),
    origin: Some(CatalogOrigin {
      channel: channel.into(),
      publication_id: Some("fixture-publication".into()),
      built_at: None,
    }),
    current,
  }
}

#[test]
fn each_physical_build_has_a_row_with_its_source_for_one_logical_component() {
  let manifests = [
    manifest('a', Source::Ci, false),
    manifest('b', Source::Release, false),
    manifest('c', Source::Local, false),
    manifest('d', Source::Local, true),
  ];
  let snapshot = snapshot(&manifests);
  let text = render(&snapshot, false);
  assert_eq!(build_rows(&snapshot).len(), 4);
  for manifest in &manifests {
    assert!(text.contains(&manifest.bundle_id[..12]));
    assert!(!text.contains(&manifest.bundle_id));
  }
  assert_eq!(text.matches("bundle-x86_64-unknown-linux-musl").count(), 4);
  assert_eq!(text.matches("provided (retained)").count(), 4);
  assert_eq!(text.matches("unknown").count(), 8); // Target suffix plus missing date.
  assert!(text.contains("0.1.3 (dirty)"));
  assert!(text.contains("dddddddddddd"));
  let rows = build_rows(&snapshot);
  for (manifest, source) in manifests.iter().zip(["ci", "release", "local", "local"]) {
    assert_eq!(
      rows[&manifest.bundle_id].cells(&snapshot, &[&manifest.bundle_id])[3],
      source
    );
  }
}

#[test]
fn activation_rollback_and_channel_heads_have_independent_precise_markers() {
  let manifests = [
    manifest('a', Source::Ci, false),
    manifest('b', Source::Release, false),
    manifest('c', Source::Ci, false),
    manifest('d', Source::Local, false),
  ];
  let mut snapshot = snapshot(&manifests);
  snapshot.active = Some(manifests[0].bundle_id.clone());
  snapshot.previous = Some(manifests[1].bundle_id.clone());
  snapshot.installations[2] = entry(&manifests[2], "nightly", true);
  snapshot.installations[3] = entry(&manifests[3], "provided", false);
  let rows = build_rows(&snapshot);
  assert_eq!(rows[&manifests[0].bundle_id].states(&snapshot), ["active"]);
  assert_eq!(
    rows[&manifests[1].bundle_id].states(&snapshot),
    ["previous"]
  );
  assert_eq!(rows[&manifests[2].bundle_id].states(&snapshot), ["current"]);
  assert_eq!(
    rows[&manifests[3].bundle_id].states(&snapshot),
    ["retained"]
  );
  let text = render(&snapshot, false);
  assert!(text.contains(&format!("Active: {}", &manifests[0].bundle_id[..12])));
  assert!(text.contains(&format!("Previous: {}", &manifests[1].bundle_id[..12])));
  assert!(text.contains("nightly (current)"));
  assert!(text.contains("provided (retained)"));
}

#[test]
fn a_shared_payload_appears_once_with_its_source_and_each_recorded_channel() {
  let manifest = manifest('a', Source::Ci, false);
  let mut snapshot = snapshot(std::slice::from_ref(&manifest));
  snapshot.active = Some(manifest.bundle_id.clone());
  let mut current = entry(&manifest, "nightly", true);
  current.origin.as_mut().unwrap().built_at = Some("2026-01-02T03:04:05Z".into());
  snapshot.installations = vec![current, entry(&manifest, "stable", false)];
  let rows = build_rows(&snapshot);
  assert_eq!(rows.len(), 1);
  let row = &rows[&manifest.bundle_id];
  let cells = row.cells(&snapshot, &[&manifest.bundle_id]);
  assert_eq!(cells[3], "ci");
  assert_eq!(cells[4], "nightly (current), stable (retained)");
  assert_eq!(cells[6], "2026-01-02T03:04:05Z");
  assert_eq!(cells[7], "active, current");
  let prefix = &manifest.bundle_id[..12];
  assert_eq!(
    render(&snapshot, false)
      .lines()
      .filter(|line| line.split_whitespace().next() == Some(prefix))
      .count(),
    1
  );
}

#[test]
fn older_remote_metadata_keeps_unknown_builds_in_the_fallback_catalog() {
  let active = manifest('a', Source::Local, true);
  let retained = entry(&manifest('b', Source::Ci, false), "provided", false);
  let catalog = InstallationCatalog::new(vec![retained.clone()]);
  let snapshot = InstallationShow::from_status(
    InstallationStatus {
      active: Some(active.clone()),
      previous: None,
    },
    Some(catalog),
  )
  .unwrap();
  let rows = build_rows(&snapshot);
  assert_eq!(rows.len(), 2);
  let unknown =
    rows[&retained.bundle_id].cells(&snapshot, &[&active.bundle_id, &retained.bundle_id]);
  assert_eq!(unknown[1], "unknown");
  assert_eq!(unknown[2], "unknown");
  assert_eq!(unknown[3], "unknown");
  assert_eq!(unknown[4], "provided (retained)");
  assert_eq!(unknown[6], "unknown");
  assert_eq!(unknown[7], "retained");
  assert!(render(&snapshot, false).contains("0.1.3 (dirty)"));
  assert_eq!(rows[&active.bundle_id].channels(), "unknown");
}

#[test]
fn short_content_ids_expand_when_prefixes_collide() {
  let first = format!("{}{}", "a".repeat(12), "b".repeat(52));
  let second = format!("{}{}", "a".repeat(12), "c".repeat(52));
  let ids = [first.as_str(), second.as_str()];
  assert_eq!(short_id(&first, &ids), format!("{}b", "a".repeat(12)));
  assert_eq!(short_id(&second, &ids), format!("{}c", "a".repeat(12)));
}
