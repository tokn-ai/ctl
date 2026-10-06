use super::*;
use serde_json::{Value, json};

pub(super) const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
pub(super) const VERSION: &str = "0.1.0";
pub(super) const TARGET: &str = "aarch64-apple-darwin";

pub(super) fn build() -> ComponentBuildInfo {
  ComponentBuildInfo {
    version: VERSION.into(),
    source_revision: Some(REVISION.into()),
    source_fingerprint: "a".repeat(64),
    dirty: false,
  }
}

fn run(id: u64, revision: &str, status: &str, conclusion: Option<&str>) -> Value {
  json!({"databaseId": id, "headSha": revision, "status": status, "conclusion": conclusion})
}

#[test]
fn selects_completed_exact_revision_runs_and_prefers_success() {
  let runs = json!([
    run(1, &"a".repeat(40), "completed", Some("success")),
    run(2, REVISION, "in_progress", Some("success")),
    run(3, REVISION, "completed", Some("failure")),
    run(4, REVISION, "completed", None),
    run(5, REVISION, "completed", Some("cancelled")),
    run(6, REVISION, "completed", Some("timed_out")),
    run(7, REVISION, "completed", Some("skipped")),
    run(101, REVISION, "completed", Some("success")),
  ]);
  assert_eq!(
    select_runs(&serde_json::to_vec(&runs).unwrap(), REVISION).unwrap(),
    vec![101, 3]
  );
  assert!(matches!(
    select_runs(b"[]", REVISION),
    Err(remote_bundle::Error::NotAvailable(_))
  ));
}

#[test]
fn workflow_list_is_strict_and_bounded() {
  for value in [
    json!({}),
    json!([{"databaseId": "101", "headSha": REVISION, "status": "completed", "conclusion": "success"}]),
    json!([{"databaseId": 101, "headSha": REVISION, "status": "completed", "conclusion": "success", "unexpected": true}]),
    json!([run(0, REVISION, "completed", Some("success"))]),
    json!([run(101, "invalid SHA", "completed", Some("success"))]),
    json!([run(101, REVISION, &"a".repeat(65), Some("success"))]),
    json!(vec![run(101, REVISION, "completed", Some("success")); 21]),
  ] {
    assert!(matches!(
      select_runs(&serde_json::to_vec(&value).unwrap(), REVISION),
      Err(remote_bundle::Error::Invalid(_))
    ));
  }
}

#[test]
fn recovery_falls_back_only_when_the_release_is_absent_or_stale() {
  assert!(permits_artifact_fallback(
    &remote_bundle::Error::NotAvailable("missing".into())
  ));
  assert!(permits_artifact_fallback(&remote_bundle::Error::Stale(
    "other revision".into()
  )));
  assert!(!permits_artifact_fallback(&remote_bundle::Error::Invalid(
    "checksum rejected".into()
  )));
  assert!(!permits_artifact_fallback(&remote_bundle::Error::Download(
    "network failure".into()
  )));
  let error = unavailable_after_release(
    &remote_bundle::Error::NotAvailable("unpublished".into()),
    &remote_bundle::Error::Download("gh unavailable".into()),
  )
  .to_string();
  assert!(error.contains("unpublished"));
  assert!(error.contains("gh unavailable"));
  assert!(error.contains("pnpm agents:sync"));
}

#[tokio::test]
async fn explicit_missing_or_stale_local_bundles_do_not_reach_download_fallback() {
  let directory = TemporaryDirectory::new().unwrap();
  assert!(matches!(
    local_bundle(TARGET, &build(), vec![directory.0.clone()], true).await,
    Err(Error::Bundle(remote_bundle::Error::NotAvailable(_)))
  ));
  assert!(
    local_bundle(TARGET, &build(), vec![directory.0.clone()], false)
      .await
      .unwrap()
      .is_none()
  );
  let mut value = bundle_manifest(b"archive");
  value["git_revision"] = json!("a".repeat(40));
  std::fs::write(
    directory.0.join("bundle-set.json"),
    serde_json::to_vec(&value).unwrap(),
  )
  .unwrap();
  std::fs::write(
    directory
      .0
      .join(value["targets"][TARGET]["archive"].as_str().unwrap()),
    b"archive",
  )
  .unwrap();
  assert!(matches!(
    local_bundle(TARGET, &build(), vec![directory.0.clone()], false).await,
    Ok(None)
  ));
  assert!(matches!(
    local_bundle(TARGET, &build(), vec![directory.0.clone()], true).await,
    Err(Error::Bundle(remote_bundle::Error::NotAvailable(_)))
  ));
}

pub(super) fn bundle_manifest(archive: &[u8]) -> Value {
  use sha2::{Digest as _, Sha256};
  let checksum = format!("{:x}", Sha256::digest(archive));
  let targets = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
  ]
  .into_iter()
  .map(|target| {
    (
      target.into(),
      json!({
        "archive": format!("ctl-agent-bundle-{VERSION}-{target}.tar.gz"), "sha256": checksum,
      }),
    )
  })
  .collect::<serde_json::Map<_, _>>();
  json!({"schema_version": 1, "app_version": VERSION, "bundle_id": VERSION, "git_revision": REVISION, "targets": targets})
}

pub(super) fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
  use std::io::{Cursor, Write as _};
  let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
  let options = zip::write::SimpleFileOptions::default()
    .compression_method(zip::CompressionMethod::Deflated)
    .unix_permissions(0o600);
  for (name, contents) in entries {
    writer.start_file(*name, options).unwrap();
    writer.write_all(contents).unwrap();
  }
  writer.finish().unwrap().into_inner()
}

#[cfg(unix)]
pub(super) fn gh_fixture(directory: &std::path::Path, script: &str) -> PathBuf {
  // Execute an immutable launcher: parallel forks can inherit a writable script
  // descriptor and cause ETXTBSY on Linux, even after the writer closes it.
  std::fs::write(directory.join("script.sh"), script).unwrap();
  let program = directory.join("gh");
  std::os::unix::fs::symlink(
    concat!(
      env!("CARGO_MANIFEST_DIR"),
      "/src/remote/bundle/fixtures/gh.sh"
    ),
    &program,
  )
  .unwrap();
  program
}

#[cfg(unix)]
struct FakeGh {
  directory: TemporaryDirectory,
  program: PathBuf,
}

#[cfg(unix)]
impl FakeGh {
  fn new(runs: &Value, manifest: &Value, archive: &[u8]) -> Self {
    let directory = TemporaryDirectory::new().unwrap();
    let manifest = serde_json::to_vec(manifest).unwrap();
    let name = format!("ctl-agent-bundle-{VERSION}-{TARGET}.tar.gz");
    let payload = zip_bytes(&[("bundle-set.json", &manifest), (&name, archive)]);
    std::fs::write(directory.0.join("payload.zip"), &payload).unwrap();
    let metadata = json!({"total_count": 1, "artifacts": [{
      "id": 202, "name": "ctl-agent-bundle-set", "size_in_bytes": payload.len(), "expired": false,
    }]});
    std::fs::write(
      directory.0.join("artifacts.json"),
      serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    std::fs::write(
      directory.0.join("runs.json"),
      serde_json::to_vec(runs).unwrap(),
    )
    .unwrap();
    let runs_path = quote(&directory.0.join("runs.json"));
    let metadata_path = quote(&directory.0.join("artifacts.json"));
    let payload_path = quote(&directory.0.join("payload.zip"));
    let log = quote(&directory.0.join("arguments"));
    let downloaded = quote(&directory.0.join("downloaded"));
    let program = gh_fixture(
      &directory.0,
      &format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$@" >> {log}
case "$1 $2" in
  'run list')
    test "$3" = '--workflow'
    test "$4" = 'bundles.yml'
    test "$5" = '--commit'
    test "$6" = '{REVISION}'
    test "$7" = '--status'
    test "$8" = 'completed'
    test "$9" = '--limit'
    test "${{10}}" = '20'
    test "${{11}}" = '--repo'
    test "${{12}}" = '{REPOSITORY}'
    test "${{13}}" = '--json'
    test "${{14}}" = 'databaseId,headSha,status,conclusion'
    cat {runs_path}
    ;;
  'api --hostname')
    test "$3" = 'github.com'
    test "$4" = '--method'
    test "$5" = 'GET'
    case "$6" in
      'repos/tokn-ai/ctl/actions/runs/102/artifacts?per_page=100')
        printf '%s' '{{"total_count":0,"artifacts":[]}}'
        ;;
      'repos/tokn-ai/ctl/actions/runs/101/artifacts?per_page=100')
        test "$7" = '--jq'
        test "$8" = '{{total_count,artifacts:[.artifacts[]|{{id,name,size_in_bytes,expired}}]}}'
        cat {metadata_path}
        ;;
      'repos/tokn-ai/ctl/actions/artifacts/202/zip')
        test "$#" = '6'
        printf '%s' yes > {downloaded}
        cat {payload_path}
        ;;
      *) exit 92 ;;
    esac
    ;;
  *) exit 91 ;;
esac
"#
      ),
    );
    Self { directory, program }
  }
}

#[cfg(unix)]
fn quote(path: &std::path::Path) -> String {
  format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(unix)]
#[tokio::test]
async fn existing_exact_revision_artifact_is_verified_without_workflow_dispatch() {
  let candidates = json!([
    run(8, &"a".repeat(40), "completed", Some("success")),
    run(101, REVISION, "completed", Some("success")),
  ]);
  let fake = FakeGh::new(&candidates, &bundle_manifest(b"archive"), b"archive");
  let bundle = download_artifact_bundle(TARGET, &build(), fake.program.as_os_str())
    .await
    .unwrap();
  assert_eq!(bundle.archive, b"archive");
  assert_eq!(bundle.git_revision, REVISION);
  let arguments = std::fs::read_to_string(fake.directory.0.join("arguments")).unwrap();
  assert!(!arguments.contains("dispatch"));
  assert!(!arguments.contains("workflow\nrun"));
}

#[cfg(unix)]
#[tokio::test]
async fn completed_failed_workflow_can_supply_a_verified_remote_bundle_set() {
  let candidates = json!([run(101, REVISION, "completed", Some("failure"))]);
  let fake = FakeGh::new(&candidates, &bundle_manifest(b"archive"), b"archive");
  let bundle = download_artifact_bundle(TARGET, &build(), fake.program.as_os_str())
    .await
    .unwrap();
  assert_eq!(bundle.archive, b"archive");
}

#[cfg(unix)]
#[tokio::test]
async fn a_missing_successful_run_artifact_allows_a_verified_failed_run() {
  let candidates = json!([
    run(101, REVISION, "completed", Some("failure")),
    run(102, REVISION, "completed", Some("success")),
  ]);
  let fake = FakeGh::new(&candidates, &bundle_manifest(b"archive"), b"archive");
  let bundle = download_artifact_bundle(TARGET, &build(), fake.program.as_os_str())
    .await
    .unwrap();
  assert_eq!(bundle.archive, b"archive");
  let arguments = std::fs::read_to_string(fake.directory.0.join("arguments")).unwrap();
  assert!(arguments.find("runs/102/").unwrap() < arguments.find("runs/101/").unwrap());
}

#[cfg(unix)]
#[tokio::test]
async fn artifact_checksum_and_revision_failures_are_not_accepted() {
  let candidates = json!([run(101, REVISION, "completed", Some("success"))]);
  let fake = FakeGh::new(&candidates, &bundle_manifest(b"trusted"), b"changed");
  let error = download_artifact_bundle(TARGET, &build(), fake.program.as_os_str())
    .await
    .unwrap_err();
  assert!(matches!(error, remote_bundle::Error::Invalid(_)), "{error}");
  let mut manifest = bundle_manifest(b"archive");
  manifest["git_revision"] = json!("a".repeat(40));
  let fake = FakeGh::new(&candidates, &manifest, b"archive");
  let error = download_artifact_bundle(TARGET, &build(), fake.program.as_os_str())
    .await
    .unwrap_err();
  assert!(matches!(error, remote_bundle::Error::Stale(_)), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn dirty_builds_do_not_invoke_github_and_nonmatching_runs_do_not_download() {
  let candidates = json!([run(101, &"a".repeat(40), "completed", Some("success"))]);
  let fake = FakeGh::new(&candidates, &bundle_manifest(b"archive"), b"archive");
  let mut expected = build();
  expected.dirty = true;
  assert!(matches!(
    download_artifact_bundle(TARGET, &expected, fake.program.as_os_str()).await,
    Err(remote_bundle::Error::Stale(_))
  ));
  assert!(!fake.directory.0.join("arguments").exists());
  assert!(matches!(
    download_artifact_bundle(TARGET, &build(), fake.program.as_os_str()).await,
    Err(remote_bundle::Error::NotAvailable(_))
  ));
  assert!(!fake.directory.0.join("downloaded").exists());
}

#[tokio::test]
async fn unavailable_github_cli_has_a_specific_diagnostic() {
  let directory = TemporaryDirectory::new().unwrap();
  let missing = directory.0.join("no-gh");
  let error = run_gh(missing.as_os_str(), &[], Duration::from_secs(1))
    .await
    .unwrap_err();
  assert!(error.to_string().contains("GitHub CLI is unavailable"));
}

#[cfg(unix)]
#[tokio::test]
async fn stalled_or_oversized_github_commands_are_terminated() {
  let directory = TemporaryDirectory::new().unwrap();
  let program = gh_fixture(&directory.0, "exec sleep 30\n");
  let error = run_gh(program.as_os_str(), &[], Duration::from_millis(100))
    .await
    .unwrap_err();
  assert!(error.to_string().contains("deadline"));
  std::fs::write(directory.0.join("script.sh"), "exec /usr/bin/yes\n").unwrap();
  let error = run_gh(program.as_os_str(), &[], Duration::from_secs(2))
    .await
    .unwrap_err();
  assert!(error.to_string().contains("size limit"));
}

#[cfg(unix)]
#[tokio::test]
async fn writable_fixture_scripts_do_not_prevent_command_execution() {
  let directory = TemporaryDirectory::new().unwrap();
  let program = gh_fixture(&directory.0, "printf '%s' \"$1\"\n");
  // Deterministically model a writable descriptor inherited by another child.
  let _writer = std::fs::OpenOptions::new()
    .write(true)
    .open(directory.0.join("script.sh"))
    .unwrap();
  let output = run_gh(
    program.as_os_str(),
    &[OsStr::new("fixture")],
    Duration::from_secs(2),
  )
  .await
  .unwrap();
  assert_eq!(output, b"fixture");
}

#[cfg(unix)]
#[tokio::test]
async fn selected_complete_upload_build_stays_stable_and_damage_is_an_error() {
  use ctl_core::bundles::{Manifest, Purpose, Source, Store};
  use ctl_core::component::ComponentInfo;
  let home = TemporaryDirectory::new().unwrap();
  let store = Store::new(&home.0);
  let ctmux = vec![
    ctmux_proto::protocol_info(),
    ctmux_ipc::local_control_protocol_info(),
  ];
  let task = vec![
    ctl_task_proto::protocol_info(),
    ctl_task_proto::control::protocol_info(),
  ];
  let ctld = ctl_ipc::lifecycle::DaemonBinaryInfo::current().protocols;
  let mut agent = ctl_proto::agent_protocols();
  agent.extend(ctmux.clone());
  agent.extend(task.clone());
  agent.extend(ctld.iter().filter(|p| p.name == "ctld").cloned());
  agent.push(ctl_core::component::ProtocolInfo::new(
    "ctl_remote_vpn",
    ctl_ipc::remote_vpn::PROTOCOL_BUILD,
    ctl_ipc::remote_vpn::PROTOCOL_VERSION,
    ctl_ipc::remote_vpn::SUPPORTED_PROTOCOL_VERSIONS,
  ));
  let mut task = task;
  task.extend(ctmux.clone());
  let components = std::collections::BTreeMap::from([
    (
      "ctl-agent".into(),
      ComponentInfo {
        build: build(),
        protocols: agent,
      },
    ),
    (
      "ctmuxd".into(),
      ComponentInfo {
        build: build(),
        protocols: ctmux,
      },
    ),
    (
      "ctl-taskd".into(),
      ComponentInfo {
        build: build(),
        protocols: task,
      },
    ),
    (
      "ctld".into(),
      ComponentInfo {
        build: build(),
        protocols: ctld,
      },
    ),
  ]);
  assert!(selected_upload(&home.0, TARGET).await.unwrap().is_none());
  let files = ctl_core::bundles::COMPONENTS
    .into_iter()
    .map(|name| (name.into(), name.as_bytes().to_vec()))
    .collect();
  let manifest = Manifest::new(TARGET, Source::Ci, components, &files).unwrap();
  let first = store.publish(&manifest, &files).unwrap();
  store.select(Purpose::Upload, &first).unwrap();
  let mut changed = files;
  changed.insert("ctmuxd".into(), b"new build".to_vec());
  let manifest = Manifest::new(
    TARGET,
    Source::Ci,
    first.manifest.components.clone(),
    &changed,
  )
  .unwrap();
  store.publish(&manifest, &changed).unwrap();
  assert_eq!(
    selected_upload(&home.0, TARGET)
      .await
      .unwrap()
      .unwrap()
      .bundle_id,
    first.manifest.bundle_id
  );
  std::fs::write(first.directory.join("ctmuxd"), b"damaged selection").unwrap();
  assert!(selected_upload(&home.0, TARGET).await.is_err());
}
