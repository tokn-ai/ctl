use super::*;
use serde_json::{Value, json};

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const BUNDLE_ID: &str = "0.1.3-nightly.20261112+g0123456789ab";

fn workflow() -> Value {
  json!({
    "id": 10,
    "path": ".github/workflows/bundles.yml",
    "event": "push",
    "run_attempt": 2,
    "status": "completed",
    "conclusion": "success",
    "repository": { "id": 1, "full_name": REPOSITORY },
    "head_repository": { "id": 1, "full_name": REPOSITORY },
    "head_branch": "main",
    "head_sha": REVISION
  })
}

#[test]
fn binds_the_desktop_artifact_to_the_exact_official_main_build() {
  let valid = workflow();
  let run: WorkflowRun = serde_json::from_value(valid.clone()).unwrap();
  assert!(run.matches(10, 2, REVISION));
  for (key, value) in [
    ("id", json!(11)),
    ("path", json!(".github/workflows/ci.yml")),
    ("event", json!("pull_request")),
    ("run_attempt", json!(3)),
    ("head_branch", json!("feature")),
    ("head_sha", json!("0".repeat(40))),
    (
      "repository",
      json!({ "id": 1, "full_name": "example/other" }),
    ),
    (
      "head_repository",
      json!({ "id": 2, "full_name": REPOSITORY }),
    ),
  ] {
    let mut bad = valid.clone();
    bad[key] = value;
    let run: WorkflowRun = serde_json::from_value(bad).unwrap();
    assert!(!run.matches(10, 2, REVISION), "{key}");
  }
}

#[test]
fn accepts_an_uploaded_target_while_other_jobs_run_or_fail() {
  for (status, conclusion) in [
    ("in_progress", Value::Null),
    ("completed", json!("failure")),
    ("completed", json!("cancelled")),
  ] {
    let mut value = workflow();
    value["status"] = json!(status);
    value["conclusion"] = conclusion;
    let run: WorkflowRun = serde_json::from_value(value).unwrap();
    assert!(run.matches(10, 2, REVISION));
  }
}

#[test]
fn rejects_expired_or_mismatched_target_artifacts() {
  let run: WorkflowRun = serde_json::from_value(workflow()).unwrap();
  let name = "ctmux-desktop-aarch64-apple-darwin";
  let valid = json!({
    "id": 20,
    "name": name,
    "expired": false,
    "workflow_run": {
      "id": 10,
      "repository_id": 1,
      "head_repository_id": 1,
      "head_branch": "main",
      "head_sha": REVISION
    }
  });
  let artifact: Artifact = serde_json::from_value(valid.clone()).unwrap();
  assert!(artifact.matches(name, &run));
  for (key, value) in [
    ("id", json!(0)),
    ("expired", json!(true)),
    ("name", json!("ctmux-desktop-x86_64-apple-darwin")),
    ("workflow_run", Value::Null),
  ] {
    let mut bad = valid.clone();
    bad[key] = value;
    let artifact: Artifact = serde_json::from_value(bad).unwrap();
    assert!(!artifact.matches(name, &run), "{key}");
  }
  let mut other_run = valid;
  other_run["workflow_run"]["head_sha"] = json!("0".repeat(40));
  let artifact: Artifact = serde_json::from_value(other_run).unwrap();
  assert!(!artifact.matches(name, &run));
}

#[test]
fn resolves_workflow_identity_from_the_current_catalog_publication() {
  let release = published::PublishedRelease {
    channel: BundleChannel::Nightly,
    publication_id: format!("{BUNDLE_ID}-10-2"),
    bundle_id: BUNDLE_ID.into(),
    app_version: "0.1.3".into(),
    git_revision: REVISION.into(),
    current: true,
  };
  assert_eq!(publication_run(&release).unwrap(), (10, 2));
  for publication_id in [
    format!("{BUNDLE_ID}-0-2"),
    format!("{BUNDLE_ID}-10-0"),
    format!("{BUNDLE_ID}-other-2"),
  ] {
    let invalid = published::PublishedRelease {
      publication_id,
      ..release.clone()
    };
    assert!(publication_run(&invalid).is_err());
  }
}

#[test]
fn windows_nightly_packages_are_bound_to_the_published_target_and_revision() {
  let run: WorkflowRun = serde_json::from_value(workflow()).unwrap();
  for target in [DesktopTarget::WindowsX64, DesktopTarget::WindowsArm64] {
    let directory = DownloadedArtifact::new().unwrap();
    let name = format!("ctmux-desktop-{}", target.triple());
    let artifact: Artifact = serde_json::from_value(json!({
      "id": 20, "name": name, "expired": false,
      "workflow_run": { "id": 10, "repository_id": 1, "head_repository_id": 1, "head_branch": "main", "head_sha": REVISION }
    })).unwrap();
    assert!(artifact.matches(&name, &run));
    assert!(!artifact.matches("ctmux-desktop-aarch64-apple-darwin", &run));
    let manifest = directory
      .0
      .join(format!("desktop-{}.json", target.triple()));
    let mut value = json!({
      "schema_version": 1, "target": target.triple(), "bundle_id": BUNDLE_ID,
      "git_revision": REVISION, "signing_mode": "not_applicable",
      "assets": [{ "name": format!("ctmux-{BUNDLE_ID}-{}.zip", target.triple()), "sha256": "a".repeat(64) }]
    });
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    let (signing, asset) = artifact_manifest(&directory.0, target, BUNDLE_ID, REVISION).unwrap();
    assert_eq!(signing, DesktopSigning::NotApplicable);
    assert_eq!(
      asset.name,
      format!("ctmux-{BUNDLE_ID}-{}.zip", target.triple())
    );
    value["git_revision"] = json!("0".repeat(40));
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(artifact_manifest(&directory.0, target, BUNDLE_ID, REVISION).is_err());
  }
}

#[test]
fn artifact_sources_must_be_regular_files() {
  let directory = DownloadedArtifact::new().unwrap();
  assert!(open_package(&directory.0).is_err());
  #[cfg(unix)]
  {
    let source = directory.0.join("package.zip");
    fs::write(&source, b"payload").unwrap();
    let link = directory.0.join("linked.zip");
    std::os::unix::fs::symlink(&source, &link).unwrap();
    assert!(open_package(&link).is_err());
  }
}

#[test]
fn hashes_the_selected_artifact_when_copying_it_to_staging() {
  let directory = DownloadedArtifact::new().unwrap();
  let payload = b"nightly desktop package";
  let asset = Asset {
    name: "ctmux-0.1.3-nightly.20261112+g0123456789ab-aarch64-apple-darwin.app.tar.gz".into(),
    sha256: format!("{:x}", Sha256::digest(payload)),
  };
  fs::write(directory.0.join(&asset.name), payload).unwrap();
  let mut target = File::create(directory.0.join("staged")).unwrap();
  directory.copy_verified(&asset, &mut target).unwrap();
  assert_eq!(fs::read(directory.0.join("staged")).unwrap(), payload);
  fs::write(directory.0.join(&asset.name), b"changed package").unwrap();
  assert!(directory.copy_verified(&asset, &mut target).is_err());
}

#[test]
fn accepts_canonical_and_explicit_unsigned_nightly_packages() {
  let directory = DownloadedArtifact::new().unwrap();
  let target = DesktopTarget::MacArm64;
  let canonical = directory.0.join("desktop-aarch64-apple-darwin.json");
  let unsigned = directory
    .0
    .join("desktop-aarch64-apple-darwin-unsigned.json");
  let value = |suffix, signing| {
    json!({
      "schema_version": 1,
      "target": target.triple(),
      "bundle_id": BUNDLE_ID,
      "git_revision": REVISION,
      "signing_mode": signing,
      "assets": [{
        "name": format!("ctmux-{BUNDLE_ID}-{}{suffix}.app.tar.gz", target.triple()),
        "sha256": "a".repeat(64)
      }]
    })
  };
  fs::write(
    &canonical,
    serde_json::to_vec(&value("", "unsigned")).unwrap(),
  )
  .unwrap();
  let (signing, asset) = artifact_manifest(&directory.0, target, BUNDLE_ID, REVISION).unwrap();
  assert_eq!(signing, DesktopSigning::Unsigned);
  assert!(!asset.name.ends_with("-unsigned.app.tar.gz"));
  fs::remove_file(&canonical).unwrap();
  fs::write(
    &unsigned,
    serde_json::to_vec(&value("-unsigned", "unsigned")).unwrap(),
  )
  .unwrap();
  let (signing, asset) = artifact_manifest(&directory.0, target, BUNDLE_ID, REVISION).unwrap();
  assert_eq!(signing, DesktopSigning::Unsigned);
  assert!(asset.name.ends_with("-unsigned.app.tar.gz"));
  fs::write(
    &unsigned,
    serde_json::to_vec(&value("-unsigned", "signed")).unwrap(),
  )
  .unwrap();
  assert!(artifact_manifest(&directory.0, target, BUNDLE_ID, REVISION).is_err());
  fs::write(
    &unsigned,
    serde_json::to_vec(&value("-unsigned", "unsigned")).unwrap(),
  )
  .unwrap();
  fs::write(&canonical, b"invalid canonical manifest").unwrap();
  assert!(artifact_manifest(&directory.0, target, BUNDLE_ID, REVISION).is_err());
}
