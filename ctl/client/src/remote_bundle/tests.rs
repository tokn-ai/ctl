use super::*;
use serde_json::{Value, json};

pub(super) const VERSION: &str = "0.1.0";
pub(super) const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
pub(super) const TARGET: &str = "aarch64-apple-darwin";

pub(super) fn build() -> ComponentBuildInfo {
  ComponentBuildInfo {
    version: VERSION.into(),
    source_revision: Some(REVISION.into()),
    source_fingerprint: "a".repeat(64),
    dirty: false,
  }
}

pub(super) fn manifest(archive: &[u8]) -> Value {
  let digest = format!("{:x}", Sha256::digest(archive));
  let targets = SUPPORTED_TARGETS
    .into_iter()
    .map(|target| {
      (
        target.into(),
        json!({
          "archive": format!("ctl-agent-bundle-{VERSION}-{target}.tar.gz"),
          "sha256": digest,
        }),
      )
    })
    .collect::<serde_json::Map<_, _>>();
  json!({
    "schema_version": 1,
    "app_version": VERSION,
    "bundle_id": VERSION,
    "git_revision": REVISION,
    "targets": targets,
  })
}

fn parse(value: &Value) -> Result<BundleSet, Error> {
  BundleSet::parse(&serde_json::to_vec(value).unwrap(), VERSION)
}

struct Directory(PathBuf);

impl Directory {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctl-remote-bundle-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }

  fn install(&self, value: &Value, archive: &[u8]) {
    std::fs::write(
      self.0.join(BUNDLE_SET_FILE),
      serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
    let path = value["targets"][TARGET]["archive"].as_str().unwrap();
    std::fs::write(self.0.join(path), archive).unwrap();
  }
}

impl Drop for Directory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[test]
fn platform_probe_maps_only_supported_unix_targets() {
  for (os, architecture, expected) in [
    ("Linux", "x86_64", "x86_64-unknown-linux-musl"),
    ("Linux", "amd64", "x86_64-unknown-linux-musl"),
    ("Linux", "aarch64", "aarch64-unknown-linux-musl"),
    ("Linux", "arm64", "aarch64-unknown-linux-musl"),
    ("Darwin", "x86_64", "x86_64-apple-darwin"),
    ("Darwin", "arm64", "aarch64-apple-darwin"),
  ] {
    let platform =
      Platform::parse_probe(&format!("ctl-platform-v1\n{os}\n{architecture}\n")).unwrap();
    assert_eq!(platform.target_triple().unwrap(), expected);
  }
  let platform = Platform::parse_probe("ctl-platform-v1\nFreeBSD\nx86_64\n").unwrap();
  assert!(matches!(
    platform.target_triple(),
    Err(Error::UnsupportedTarget(_))
  ));
}

#[test]
fn platform_probe_rejects_missing_fields_and_unremoved_startup_text() {
  for output in [
    "",
    "ctl-platform-v1\nLinux\n",
    "ctl-platform-v1\n\nx86_64\n",
    "banner\nctl-platform-v1\nLinux\nx86_64\n",
    "ctl-platform-v1\nLinux\nx86_64\nextra\n",
  ] {
    assert!(matches!(
      Platform::parse_probe(output),
      Err(Error::Invalid(_))
    ));
  }
}

#[test]
fn valid_release_and_revision_bound_development_sets_are_supported() {
  let mut value = manifest(b"archive");
  parse(&value).unwrap().verify_revision(&build()).unwrap();
  let development_id = format!("{VERSION}-dev.{}", &REVISION[..12]);
  value["bundle_id"] = json!(development_id);
  for target in SUPPORTED_TARGETS {
    value["targets"][target]["archive"] =
      json!(format!("ctl-agent-bundle-{development_id}-{target}.tar.gz"));
  }
  let parsed = parse(&value).unwrap();
  assert_eq!(parsed.bundle_id, development_id);
  parsed.verify_revision(&build()).unwrap();
}

#[test]
fn archive_accessors_return_only_validated_canonical_names() {
  let set = parse(&manifest(b"archive")).unwrap();
  let names: Vec<_> = set.archive_names().collect();
  assert_eq!(names.len(), 4);
  for target in SUPPORTED_TARGETS {
    let expected = format!("ctl-agent-bundle-{VERSION}-{target}.tar.gz");
    assert_eq!(set.archive_name(target).unwrap(), expected);
    assert!(names.contains(&expected.as_str()));
  }
  assert!(matches!(
    set.archive_name("x86_64-pc-windows-msvc"),
    Err(Error::UnsupportedTarget(_))
  ));
}

#[test]
fn malformed_manifests_and_unknown_fields_are_rejected() {
  assert!(matches!(
    BundleSet::parse(b"not JSON", VERSION),
    Err(Error::Invalid(_))
  ));
  for (field, replacement) in [
    ("schema_version", json!(2)),
    ("app_version", json!("999.0.0")),
    ("bundle_id", json!("../escape")),
    ("bundle_id", json!("unrelated-release")),
    ("git_revision", json!("bad revision")),
    ("git_revision", json!("z".repeat(40))),
    ("unexpected", json!(true)),
  ] {
    let mut value = manifest(b"archive");
    value[field] = replacement;
    assert!(matches!(parse(&value), Err(Error::Invalid(_))), "{field}");
  }
}

#[test]
fn unsafe_archive_names_and_malformed_checksums_never_form_paths_or_urls() {
  for name in [
    "../escape.tar.gz",
    "/absolute.tar.gz",
    "https://example.com/archive",
    "wrong.tar.gz",
  ] {
    let mut value = manifest(b"archive");
    value["targets"][TARGET]["archive"] = json!(name);
    assert!(matches!(parse(&value), Err(Error::Invalid(_))));
  }
  for digest in ["bad".to_owned(), "z".repeat(64)] {
    let mut value = manifest(b"archive");
    value["targets"][TARGET]["sha256"] = json!(digest);
    assert!(matches!(parse(&value), Err(Error::Invalid(_))));
  }
  let mut value = manifest(b"archive");
  value["targets"][TARGET]["unexpected"] = json!(true);
  assert!(matches!(parse(&value), Err(Error::Invalid(_))));
}

#[test]
fn incomplete_or_extra_target_sets_are_rejected() {
  let mut value = manifest(b"archive");
  value["targets"].as_object_mut().unwrap().remove(TARGET);
  assert!(matches!(parse(&value), Err(Error::Invalid(_))));
  value = manifest(b"archive");
  value["targets"]["x86_64-pc-windows-msvc"] = value["targets"][TARGET].clone();
  assert!(matches!(parse(&value), Err(Error::Invalid(_))));
}

#[test]
fn invalid_client_versions_are_rejected_before_release_path_construction() {
  let bytes = serde_json::to_vec(&manifest(b"archive")).unwrap();
  for version in ["", "../escape", "0.1.0\n", "https://example.com"] {
    assert!(matches!(
      BundleSet::parse(&bytes, version),
      Err(Error::Invalid(_))
    ));
  }
}

#[test]
fn source_revision_and_clean_build_state_are_required() {
  let set = parse(&manifest(b"archive")).unwrap();
  let mut expected = build();
  expected.source_revision = None;
  assert!(matches!(
    set.verify_revision(&expected),
    Err(Error::Stale(_))
  ));
  expected = build();
  expected.source_revision = Some("a".repeat(40));
  assert!(matches!(
    set.verify_revision(&expected),
    Err(Error::Stale(_))
  ));
  expected = build();
  expected.dirty = true;
  assert!(matches!(
    set.verify_revision(&expected),
    Err(Error::Stale(_))
  ));
  expected = build();
  expected.version = "0.2.0".into();
  assert!(matches!(
    set.verify_revision(&expected),
    Err(Error::Stale(_))
  ));
  expected = build();
  expected.source_fingerprint = "invalid".into();
  assert!(matches!(
    set.verify_revision(&expected),
    Err(Error::Stale(_))
  ));
}

#[test]
fn archive_checksum_is_verified_before_it_can_be_uploaded() {
  let directory = Directory::new();
  let mut value = manifest(b"trusted archive");
  // SHA-256 text is case-insensitive; payload bytes must still match exactly.
  value["targets"][TARGET]["sha256"] = json!(
    value["targets"][TARGET]["sha256"]
      .as_str()
      .unwrap()
      .to_ascii_uppercase()
  );
  directory.install(&value, b"trusted archive");
  let found = read_verified_bundle(std::slice::from_ref(&directory.0), TARGET, &build())
    .unwrap()
    .unwrap();
  assert_eq!(found.archive, b"trusted archive");
  assert_eq!(found.app_version, VERSION);
  assert_eq!(found.git_revision, REVISION);
  assert_eq!(found.manifest, serde_json::to_vec(&value).unwrap());
  assert_eq!(
    found.file_name,
    value["targets"][TARGET]["archive"].as_str().unwrap()
  );
  directory.install(&value, b"changed archive");
  assert!(matches!(
    read_verified_bundle(std::slice::from_ref(&directory.0), TARGET, &build()),
    Err(Error::Invalid(_))
  ));
}

#[test]
fn reusable_local_sets_skip_stale_schema1_payloads_and_reuse_other_clean_releases() {
  let old = Directory::new();
  old.install(&manifest(b"archive"), b"archive");
  std::fs::remove_file(
    old.0.join(
      manifest(b"archive")["targets"][TARGET]["archive"]
        .as_str()
        .unwrap(),
    ),
  )
  .unwrap();
  let compatible = Directory::new();
  let bundle = compatibility::tests::Fixture::new("0.0.9", &"b".repeat(40)).bundle();
  let metadata: Value = serde_json::from_slice(&bundle.manifest).unwrap();
  compatible.install(&metadata, &bundle.archive);
  for identity in 0..3 {
    let mut expected = build();
    match identity {
      0 => expected.source_revision = Some("a".repeat(40)),
      1 => expected.source_revision = None,
      _ => expected.dirty = true,
    }
    let reused = read_reusable_bundle(&[old.0.clone(), compatible.0.clone()], TARGET, &expected)
      .unwrap()
      .unwrap();
    assert_eq!(reused.git_revision, bundle.git_revision);
    assert_eq!(reused.app_version, "0.0.9");
    assert!(
      read_reusable_bundle(std::slice::from_ref(&old.0), TARGET, &expected)
        .unwrap()
        .is_none()
    );
  }
  // Exact schema-1 policy remains unchanged for callers that demand it.
  assert!(read_verified_bundle(std::slice::from_ref(&old.0), TARGET, &build()).is_err());
  assert!(
    read_compatible_bundle(std::slice::from_ref(&old.0), TARGET)
      .unwrap()
      .is_none()
  );
}

#[test]
fn compatible_local_selection_prefilters_only_valid_ineligible_metadata() {
  let directory = Directory::new();
  let fixture = compatibility::tests::Fixture::new("0.0.9", &"b".repeat(40));
  let mut metadata = fixture.outer.clone();
  let protocol = &mut metadata["targets"][TARGET]["components"]["ctmuxd"]["protocols"][0];
  let protocol_build = protocol["build"].as_u64().unwrap();
  protocol["version"] = json!(format!("2.0.{protocol_build}"));
  protocol["supported_versions"] = json!([format!("2.0.{protocol_build}")]);
  std::fs::write(
    directory.0.join(BUNDLE_SET_FILE),
    serde_json::to_vec(&metadata).unwrap(),
  )
  .unwrap();
  assert!(
    read_reusable_bundle(std::slice::from_ref(&directory.0), TARGET, &build())
      .unwrap()
      .is_none()
  );
  metadata["targets"][TARGET]["components"]["ctmuxd"]["build"]["dirty"] = json!(true);
  std::fs::write(
    directory.0.join(BUNDLE_SET_FILE),
    serde_json::to_vec(&metadata).unwrap(),
  )
  .unwrap();
  assert!(matches!(
    read_compatible_bundle(std::slice::from_ref(&directory.0), TARGET),
    Err(Error::Invalid(_))
  ));
  // Eligible metadata never authorizes a missing or corrupted payload.
  let bundle = fixture.bundle();
  directory.install(
    &serde_json::from_slice(&bundle.manifest).unwrap(),
    b"corrupted archive",
  );
  assert!(matches!(
    read_compatible_bundle(std::slice::from_ref(&directory.0), TARGET),
    Err(Error::Invalid(_))
  ));
}

#[test]
fn missing_directories_are_skipped_but_present_invalid_sets_are_not() {
  let absent = Directory::new();
  let invalid = Directory::new();
  let valid = Directory::new();
  valid.install(&manifest(b"archive"), b"archive");
  assert!(
    read_verified_bundle(std::slice::from_ref(&absent.0), TARGET, &build())
      .unwrap()
      .is_none()
  );
  assert!(
    read_verified_bundle(&[absent.0.clone(), valid.0.clone()], TARGET, &build())
      .unwrap()
      .is_some()
  );
  std::fs::write(invalid.0.join(BUNDLE_SET_FILE), b"invalid manifest").unwrap();
  assert!(matches!(
    read_verified_bundle(&[invalid.0.clone(), valid.0.clone()], TARGET, &build()),
    Err(Error::Invalid(_))
  ));
}

#[test]
fn stale_local_sets_do_not_fall_back_to_an_unrelated_set() {
  let stale = Directory::new();
  let valid = Directory::new();
  let mut value = manifest(b"archive");
  value["git_revision"] = json!("a".repeat(40));
  stale.install(&value, b"archive");
  valid.install(&manifest(b"archive"), b"archive");
  assert!(matches!(
    read_verified_bundle(&[stale.0.clone(), valid.0.clone()], TARGET, &build()),
    Err(Error::Stale(_))
  ));
}

#[test]
fn oversized_manifests_and_archives_are_rejected() {
  let mut bytes = serde_json::to_vec(&manifest(b"archive")).unwrap();
  bytes.resize(MAX_BUNDLE_SET_BYTES + 1, b' ');
  assert!(matches!(
    BundleSet::parse(&bytes, VERSION),
    Err(Error::Invalid(_))
  ));
  let directory = Directory::new();
  std::fs::write(directory.0.join(BUNDLE_SET_FILE), &bytes).unwrap();
  assert!(matches!(
    read_verified_bundle(std::slice::from_ref(&directory.0), TARGET, &build()),
    Err(Error::Invalid(_))
  ));
  let value = manifest(b"archive");
  directory.install(&value, b"archive");
  let archive = directory
    .0
    .join(value["targets"][TARGET]["archive"].as_str().unwrap());
  File::options()
    .write(true)
    .open(archive)
    .unwrap()
    .set_len(MAX_BUNDLE_BYTES as u64 + 1)
    .unwrap();
  assert!(matches!(
    read_verified_bundle(std::slice::from_ref(&directory.0), TARGET, &build()),
    Err(Error::Invalid(_))
  ));
}

#[test]
fn missing_archive_is_an_error_after_a_set_has_been_selected() {
  let directory = Directory::new();
  std::fs::write(
    directory.0.join(BUNDLE_SET_FILE),
    serde_json::to_vec(&manifest(b"archive")).unwrap(),
  )
  .unwrap();
  assert!(
    matches!(read_verified_bundle(std::slice::from_ref(&directory.0), TARGET, &build()), Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound)
  );
}

#[test]
fn download_redirects_allow_only_https_github_release_hosts() {
  for url in [
    "https://github.com/tokn-ai/ctl/releases/download/v0.1.0/bundle-set.json",
    "https://release-assets.githubusercontent.com/asset",
    "https://objects.githubusercontent.com/asset",
    "https://github-releases.githubusercontent.com/asset",
  ] {
    assert!(trusted_url(&reqwest::Url::parse(url).unwrap()));
  }
  for url in [
    "http://github.com/asset",
    "https://github.com.example/asset",
    "https://github.com:8443/asset",
    "https://user:password@github.com/asset",
    "https://example.com/asset",
  ] {
    assert!(!trusted_url(&reqwest::Url::parse(url).unwrap()));
  }
}

#[tokio::test]
async fn invalid_or_dirty_builds_cannot_start_release_downloads() {
  let mut expected = build();
  expected.dirty = true;
  assert!(matches!(
    download_release_bundle(TARGET, &expected).await,
    Err(Error::Stale(_))
  ));
  expected = build();
  expected.source_revision = None;
  assert!(matches!(
    download_release_bundle(TARGET, &expected).await,
    Err(Error::Stale(_))
  ));
  expected = build();
  expected.version = "../other-repository".into();
  assert!(matches!(
    download_release_bundle(TARGET, &expected).await,
    Err(Error::Invalid(_))
  ));
  assert!(matches!(
    download_release_bundle("../../archive", &build()).await,
    Err(Error::UnsupportedTarget(_))
  ));
}
