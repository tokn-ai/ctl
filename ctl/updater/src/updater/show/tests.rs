use super::*;
use ctl_client::installation::{InstallationCatalog, InstallationStatus};
use ctl_core::bundle_layout::{ALL, required_protocols};
use ctl_core::bundles::Manifest;
use ctl_core::component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo};
use ctl_core::installations::CatalogOrigin;
use ctl_core::protocol::ProtocolVersion;

fn manifest(token: char, source: Source, dirty: bool) -> Manifest {
  manifest_version(token, source, dirty, "0.1.3")
}

fn manifest_version(token: char, source: Source, dirty: bool, version: &str) -> Manifest {
  let build = ComponentBuildInfo {
    version: version.into(),
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
    schema_version: 2,
    active: None,
    active_origin: None,
    previous: None,
    missing_builds: Vec::new(),
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
  let rows = build_rows(&snapshot);
  assert_eq!(rows.len(), 4);
  assert_eq!(text.matches("bundle-x86_64-unknown-linux-musl").count(), 4);
  assert_eq!(text.matches("provided (retained)").count(), 4);
  assert!(text.contains("0.1.3 (dirty)"));
  assert!(text.contains("dddddddddddd"));
  for (manifest, source) in manifests.iter().zip(["ci", "release", "local", "local"]) {
    let row = &rows[&manifest.bundle_id];
    let label = row.label(&snapshot, &[&manifest.bundle_id]);
    assert!(text.contains(&label));
    assert!(!text.contains(&manifest.bundle_id));
    let cells = row.cells(&snapshot, label);
    assert_eq!(cells[3], source);
    assert_eq!(cells[6], "unknown");
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
  let ids = rows.keys().map(String::as_str).collect::<Vec<_>>();
  let active = rows[&manifests[0].bundle_id].label(&snapshot, &ids);
  let previous = rows[&manifests[1].bundle_id].label(&snapshot, &ids);
  assert!(text.starts_with(&format!("Active: {active}\n")));
  assert!(text.contains(&format!("\nPrevious: {previous}\n")));
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
  let label = row.label(&snapshot, &[&manifest.bundle_id]);
  let cells = row.cells(&snapshot, label.clone());
  assert_eq!(cells[3], "ci");
  assert_eq!(cells[4], "nightly (current), stable (retained)");
  assert_eq!(cells[6], "2026-01-02T03:04:05Z");
  assert_eq!(cells[7], "active, current");
  assert_eq!(
    render(&snapshot, false)
      .lines()
      .filter(|line| line.split_whitespace().next() == Some(label.as_str()))
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
      missing_builds: Vec::new(),
    },
    Some(catalog),
  )
  .unwrap();
  let rows = build_rows(&snapshot);
  assert_eq!(rows.len(), 2);
  let unknown = rows[&retained.bundle_id].cells(&snapshot, retained.bundle_id[..12].into());
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
fn missing_builds_show_recorded_memberships_and_selection() {
  let active = manifest('a', Source::Release, false);
  let retained = manifest('b', Source::Local, false);
  let mut snapshot = snapshot(&[active.clone(), retained.clone()]);
  snapshot.active = Some(active.bundle_id.clone());
  snapshot.installations[0] = entry(&active, "stable", true);
  snapshot.installations[1] = entry(&retained, "local", false);
  snapshot.builds.clear();
  snapshot.missing_builds = vec![active.bundle_id.clone(), retained.bundle_id.clone()];
  snapshot.validate().unwrap();

  let rows = build_rows(&snapshot);
  assert_eq!(
    rows[&active.bundle_id].states(&snapshot),
    ["active", "current", "missing"]
  );
  assert_eq!(
    rows[&retained.bundle_id].states(&snapshot),
    ["retained", "missing"]
  );
  let text = render(&snapshot, false);
  assert!(text.contains(&format!("Active: {} (missing)", &active.bundle_id[..12])));
  assert!(text.contains("stable (current)"));
  assert!(text.contains("bundle-x86_64-unknown-linux-musl"));
  assert!(text.contains("ctlup update"));
}

#[test]
fn short_content_ids_expand_when_prefixes_collide() {
  let first = format!("{}{}", "a".repeat(12), "b".repeat(52));
  let second = format!("{}{}", "a".repeat(12), "c".repeat(52));
  let ids = [first.as_str(), second.as_str()];
  assert_eq!(short_id(&first, &ids), format!("{}b", "a".repeat(12)));
  assert_eq!(short_id(&second, &ids), format!("{}c", "a".repeat(12)));
}

#[test]
fn known_builds_use_channel_labels_and_sort_by_version_or_build_time() {
  let manifests = [
    manifest_version('a', Source::Release, false, "0.1.9"),
    manifest_version('b', Source::Release, false, "0.1.10"),
    manifest('c', Source::Ci, false),
    manifest('d', Source::Ci, false),
    manifest('e', Source::Local, false),
    manifest('f', Source::Ci, false),
  ];
  let mut snapshot = snapshot(&manifests);
  for (index, (channel, date)) in [
    ("stable", "2026-01-08T00:00:00Z"),
    ("stable", "2026-01-02T00:00:00Z"),
    ("nightly", "2026-01-03T00:00:00Z"),
    ("nightly", "2026-01-04T00:00:00Z"),
    ("local", "2026-01-05T00:00:00Z"),
    ("pr-192", "2026-01-06T00:00:00Z"),
  ]
  .into_iter()
  .enumerate()
  {
    let mut entry = entry(&manifests[index], channel, false);
    entry.origin.as_mut().unwrap().built_at = Some(date.into());
    snapshot.installations[index] = entry;
  }

  let text = render(&snapshot, false);
  let labels = text
    .lines()
    .skip_while(|line| !line.starts_with("BUILD"))
    .skip(1)
    .map(|line| line.split_whitespace().next().unwrap())
    .collect::<Vec<_>>();
  assert_eq!(
    labels,
    [
      "stable-v0.1.10-bbbbbbb",
      "stable-v0.1.9-aaaaaaa",
      "nightly-20260104T000000Z-v0.1.3-ddddddd",
      "nightly-20260103T000000Z-v0.1.3-ccccccc",
      "dev-20260106T000000Z-v0.1.3-fffffff-pr-192",
      "dev-20260105T000000Z-v0.1.3-eeeeeee-local",
    ]
  );
}
