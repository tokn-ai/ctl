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
  assert!(matches!(
    local_bundle(TARGET, &build(), vec![directory.0.clone()], false).await,
    Err(Error::Bundle(remote_bundle::Error::Stale(_)))
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
struct FakeGh {
  directory: TemporaryDirectory,
  program: PathBuf,
}

#[cfg(unix)]
impl FakeGh {
  fn new(runs: &Value, manifest: &Value, archive: &[u8]) -> Self {
    use std::os::unix::fs::PermissionsExt as _;
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
    let program = directory.0.join("gh");
    let runs_path = quote(&directory.0.join("runs.json"));
    let metadata_path = quote(&directory.0.join("artifacts.json"));
    let payload_path = quote(&directory.0.join("payload.zip"));
    let log = quote(&directory.0.join("arguments"));
    let downloaded = quote(&directory.0.join("downloaded"));
    std::fs::write(
      &program,
      format!(
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
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
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
  assert!(matches!(
    download_artifact_bundle(TARGET, &build(), fake.program.as_os_str()).await,
    Err(remote_bundle::Error::Invalid(_))
  ));
  let mut manifest = bundle_manifest(b"archive");
  manifest["git_revision"] = json!("a".repeat(40));
  let fake = FakeGh::new(&candidates, &manifest, b"archive");
  assert!(matches!(
    download_artifact_bundle(TARGET, &build(), fake.program.as_os_str()).await,
    Err(remote_bundle::Error::Stale(_))
  ));
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
  use std::os::unix::fs::PermissionsExt as _;
  let directory = TemporaryDirectory::new().unwrap();
  let program = directory.0.join("gh");
  std::fs::write(&program, "#!/bin/sh\nexec sleep 30\n").unwrap();
  std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
  let error = run_gh(program.as_os_str(), &[], Duration::from_millis(100))
    .await
    .unwrap_err();
  assert!(error.to_string().contains("deadline"));
  std::fs::write(&program, "#!/bin/sh\nexec /usr/bin/yes\n").unwrap();
  let error = run_gh(program.as_os_str(), &[], Duration::from_secs(2))
    .await
    .unwrap_err();
  assert!(error.to_string().contains("size limit"));
}

fn verified_fixture_bundle(archive: &[u8]) -> VerifiedBundle {
  let directory = TemporaryDirectory::new().unwrap();
  std::fs::write(
    directory.0.join("bundle-set.json"),
    serde_json::to_vec(&bundle_manifest(archive)).unwrap(),
  )
  .unwrap();
  std::fs::write(
    directory
      .0
      .join(format!("ctl-agent-bundle-{VERSION}-{TARGET}.tar.gz")),
    archive,
  )
  .unwrap();
  remote_bundle::read_verified_bundle(std::slice::from_ref(&directory.0), TARGET, &build())
    .unwrap()
    .unwrap()
}

fn cache_directory(root: &std::path::Path) -> PathBuf {
  root.join(REVISION).join(TARGET)
}

fn offline_fixture_error() -> Error {
  remote_bundle::Error::Download("fixture network is offline".into()).into()
}

#[tokio::test]
async fn verified_download_is_cached_and_the_next_call_never_invokes_download() {
  use std::cell::Cell;

  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.join("cache");
  let downloads = Cell::new(0);
  let first = matching_bundle_from(TARGET, &build(), vec![], false, Some(root.clone()), || {
    downloads.set(downloads.get() + 1);
    std::future::ready(Ok(verified_fixture_bundle(b"trusted archive")))
  })
  .await
  .unwrap();
  let second = matching_bundle_from(TARGET, &build(), vec![], false, Some(root.clone()), || {
    downloads.set(downloads.get() + 1);
    std::future::ready(Err(offline_fixture_error()))
  })
  .await
  .unwrap();
  assert_eq!(downloads.get(), 1);
  assert_eq!(first.archive, b"trusted archive");
  assert_eq!(second.archive, first.archive);
  assert_eq!(second.git_revision, REVISION);
  let entry = cache_directory(&root);
  let manifest: Value =
    serde_json::from_slice(&std::fs::read(entry.join("bundle-set.json")).unwrap()).unwrap();
  assert_eq!(manifest, bundle_manifest(b"trusted archive"));
  assert_eq!(
    std::fs::read(entry.join(second.file_name)).unwrap(),
    second.archive
  );
}

#[cfg(unix)]
#[tokio::test]
async fn an_actual_ci_artifact_download_is_reused_without_github_on_the_next_call() {
  let candidates = json!([run(101, REVISION, "completed", Some("success"))]);
  let fake = FakeGh::new(&candidates, &bundle_manifest(b"CI archive"), b"CI archive");
  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.join("cache");
  let expected = build();
  let first = matching_bundle_from(
    TARGET,
    &expected,
    vec![],
    false,
    Some(root.clone()),
    || async {
      download_artifact_bundle(TARGET, &expected, fake.program.as_os_str())
        .await
        .map_err(Error::from)
    },
  )
  .await
  .unwrap();
  assert_eq!(first.archive, b"CI archive");
  assert!(fake.directory.0.join("downloaded").exists());
  let arguments = std::fs::read(fake.directory.0.join("arguments")).unwrap();
  std::fs::remove_file(&fake.program).unwrap();
  let second = matching_bundle_from(
    TARGET,
    &expected,
    vec![],
    false,
    Some(root.clone()),
    || async {
      download_artifact_bundle(TARGET, &expected, fake.program.as_os_str())
        .await
        .map_err(Error::from)
    },
  )
  .await
  .unwrap();
  assert_eq!(second.archive, first.archive);
  assert_eq!(
    std::fs::read(fake.directory.0.join("arguments")).unwrap(),
    arguments
  );
  let entry = cache_directory(&root);
  let manifest: Value =
    serde_json::from_slice(&std::fs::read(entry.join("bundle-set.json")).unwrap()).unwrap();
  assert_eq!(manifest, bundle_manifest(b"CI archive"));
  assert_eq!(
    std::fs::read(entry.join(second.file_name)).unwrap(),
    second.archive
  );
}

#[tokio::test]
async fn damaged_managed_cache_is_downloaded_again_and_replaced() {
  use std::cell::Cell;

  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.join("cache");
  let downloads = Cell::new(0);
  let download = || {
    downloads.set(downloads.get() + 1);
    std::future::ready(Ok(verified_fixture_bundle(b"trusted archive")))
  };
  matching_bundle_from(
    TARGET,
    &build(),
    vec![],
    false,
    Some(root.clone()),
    download,
  )
  .await
  .unwrap();
  for (index, file) in [
    format!("ctl-agent-bundle-{VERSION}-{TARGET}.tar.gz"),
    "bundle-set.json".into(),
  ]
  .into_iter()
  .enumerate()
  {
    std::fs::write(cache_directory(&root).join(file), b"corrupt").unwrap();
    let repaired = matching_bundle_from(
      TARGET,
      &build(),
      vec![],
      false,
      Some(root.clone()),
      download,
    )
    .await
    .unwrap();
    assert_eq!(downloads.get(), index + 2);
    assert_eq!(repaired.archive, b"trusted archive");
    let cached = matching_bundle_from(TARGET, &build(), vec![], false, Some(root.clone()), || {
      std::future::ready(Err(offline_fixture_error()))
    })
    .await
    .unwrap();
    assert_eq!(cached.archive, repaired.archive);
  }
}

#[tokio::test]
async fn an_explicit_missing_or_invalid_override_cannot_fall_back_to_valid_cache() {
  use std::cell::Cell;

  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.join("cache");
  let override_directory = directory.0.join("override");
  std::fs::create_dir(&override_directory).unwrap();
  matching_bundle_from(TARGET, &build(), vec![], false, Some(root.clone()), || {
    std::future::ready(Ok(verified_fixture_bundle(b"trusted archive")))
  })
  .await
  .unwrap();
  let original_manifest = std::fs::read(cache_directory(&root).join("bundle-set.json")).unwrap();
  for invalid in [false, true] {
    if invalid {
      std::fs::write(
        override_directory.join("bundle-set.json"),
        b"invalid override",
      )
      .unwrap();
    }
    let called = Cell::new(false);
    let result = matching_bundle_from(
      TARGET,
      &build(),
      vec![override_directory.clone()],
      true,
      Some(root.clone()),
      || {
        called.set(true);
        std::future::ready(Err(offline_fixture_error()))
      },
    )
    .await;
    assert!(!called.get());
    if invalid {
      assert!(matches!(
        result,
        Err(Error::Bundle(remote_bundle::Error::Invalid(_)))
      ));
    } else {
      assert!(matches!(
        result,
        Err(Error::Bundle(remote_bundle::Error::NotAvailable(_)))
      ));
    }
    assert_eq!(
      std::fs::read(cache_directory(&root).join("bundle-set.json")).unwrap(),
      original_manifest
    );
  }
}

#[tokio::test]
async fn an_unusable_cache_path_does_not_reject_an_already_verified_download() {
  let directory = TemporaryDirectory::new().unwrap();
  let blocked = directory.0.join("cache");
  std::fs::write(&blocked, b"existing file").unwrap();
  let bundle = matching_bundle_from(
    TARGET,
    &build(),
    vec![],
    false,
    Some(blocked.clone()),
    || std::future::ready(Ok(verified_fixture_bundle(b"trusted archive"))),
  )
  .await
  .unwrap();
  assert_eq!(bundle.archive, b"trusted archive");
  assert_eq!(std::fs::read(blocked).unwrap(), b"existing file");
}

#[tokio::test]
async fn a_cache_publication_failure_keeps_the_verified_download_usable() {
  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.clone();
  let lock = root.join(REVISION).join(format!("{TARGET}.lock"));
  let mut builder = std::fs::DirBuilder::new();
  builder.recursive(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt as _;
    builder.mode(0o700);
  }
  builder.create(&lock).unwrap();
  std::fs::write(lock.join("sentinel"), b"existing directory").unwrap();
  let bundle = matching_bundle_from(TARGET, &build(), vec![], false, Some(root.clone()), || {
    std::future::ready(Ok(verified_fixture_bundle(b"trusted archive")))
  })
  .await
  .unwrap();
  assert_eq!(bundle.archive, b"trusted archive");
  assert!(!cache_directory(&root).exists());
  assert_eq!(
    std::fs::read(lock.join("sentinel")).unwrap(),
    b"existing directory"
  );
}

#[tokio::test]
async fn a_busy_cache_publication_lock_does_not_wait_or_unlock_the_other_publisher() {
  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.clone();
  let revision = root.join(REVISION);
  let builder = std::fs::DirBuilder::new();
  #[cfg(unix)]
  let mut builder = builder;
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt as _;
    builder.mode(0o700);
  }
  builder.create(&revision).unwrap();
  let lock = revision.join(format!("{TARGET}.lock"));
  let mut options = std::fs::OpenOptions::new();
  options.read(true).write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
  }
  let publisher = options.open(&lock).unwrap();
  publisher.lock().unwrap();
  let bundle = tokio::time::timeout(
    Duration::from_secs(2),
    matching_bundle_from(TARGET, &build(), vec![], false, Some(root.clone()), || {
      std::future::ready(Ok(verified_fixture_bundle(b"trusted archive")))
    }),
  )
  .await
  .expect("an optional cache must not block on another publisher")
  .unwrap();
  assert_eq!(bundle.archive, b"trusted archive");
  assert!(!cache_directory(&root).exists());
  let contender = std::fs::OpenOptions::new()
    .read(true)
    .write(true)
    .open(lock)
    .unwrap();
  assert!(matches!(
    contender.try_lock(),
    Err(std::fs::TryLockError::WouldBlock)
  ));
}

#[tokio::test]
async fn a_failed_download_does_not_publish_a_cache_entry() {
  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.join("cache");
  let result = matching_bundle_from(TARGET, &build(), vec![], false, Some(root.clone()), || {
    std::future::ready(Err(offline_fixture_error()))
  })
  .await;
  assert!(matches!(
    result,
    Err(Error::Bundle(remote_bundle::Error::Download(_)))
  ));
  assert!(!cache_directory(&root).exists());
}

#[tokio::test]
async fn a_cancelled_download_does_not_publish_a_cache_entry() {
  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.join("cache");
  let entry = cache_directory(&root);
  let started = std::sync::Arc::new(tokio::sync::Notify::new());
  let notification = std::sync::Arc::clone(&started);
  let task = tokio::spawn(async move {
    matching_bundle_from(TARGET, &build(), vec![], false, Some(root), || async move {
      notification.notify_one();
      std::future::pending::<Result<VerifiedBundle, Error>>().await
    })
    .await
  });
  tokio::time::timeout(Duration::from_secs(2), started.notified())
    .await
    .unwrap();
  task.abort();
  assert!(task.await.unwrap_err().is_cancelled());
  assert!(!entry.exists());
}
