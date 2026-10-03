#[cfg(unix)]
use super::super::tests::gh_fixture;
use super::super::tests::{TARGET, build, bundle_manifest, zip_bytes};
use super::*;
use serde_json::json;

fn metadata(id: u64, size: u64, expired: bool) -> Vec<u8> {
  serde_json::to_vec(&json!({"total_count": 1, "artifacts": [{
    "id": id, "name": ARTIFACT_NAME, "size_in_bytes": size, "expired": expired,
  }]}))
  .unwrap()
}

#[test]
fn artifact_metadata_requires_one_complete_unexpired_bounded_match() {
  assert_eq!(select_artifact(&metadata(202, 42, false)).unwrap().id, 202);
  for bytes in [
    metadata(202, 42, true),
    br#"{"total_count":0,"artifacts":[]}"#.to_vec(),
  ] {
    assert!(matches!(
      select_artifact(&bytes),
      Err(Error::NotAvailable(_))
    ));
  }
  for bytes in [
    metadata(0, 42, false), metadata(202, 0, false), metadata(202, MAX_ZIP_BYTES + 1, false),
    br#"{"total_count":1,"artifacts":[]}"#.to_vec(),
    br#"{"total_count":0,"artifacts":[],"extra":true}"#.to_vec(),
    br#"{"total_count":2,"artifacts":[{"id":1,"name":"ctl-agent-bundle-set","size_in_bytes":42,"expired":false},{"id":2,"name":"ctl-agent-bundle-set","size_in_bytes":42,"expired":false}]}"#.to_vec(),
  ] {
    assert!(matches!(select_artifact(&bytes), Err(Error::Invalid(_))));
  }
}

fn fixture(archive: &[u8]) -> Vec<u8> {
  let manifest = serde_json::to_vec(&bundle_manifest(archive)).unwrap();
  let name = format!("ctl-agent-bundle-0.1.0-{TARGET}.tar.gz");
  zip_bytes(&[("bundle-set.json", &manifest), (&name, archive)])
}

fn extract(bytes: &[u8]) -> Result<VerifiedBundle, Error> {
  let directory = TemporaryDirectory::new().unwrap();
  let path = directory.0.join(".download.zip");
  std::fs::write(&path, bytes).unwrap();
  extract_verified(&path, &directory.0, TARGET, &build())
}

#[test]
fn a_flat_zip_supplies_only_the_verified_requested_bundle() {
  let bundle = extract(&fixture(b"archive")).unwrap();
  assert_eq!(bundle.archive, b"archive");
  assert_eq!(bundle.git_revision, build().source_revision.unwrap());
}

#[test]
fn zip_paths_special_files_and_unexpected_files_are_rejected() {
  for name in [
    "../bundle-set.json",
    "/bundle-set.json",
    "a/b",
    "a\\b",
    "C:escape",
    "unexpected",
  ] {
    assert!(matches!(
      extract(&zip_bytes(&[(name, b"x")])),
      Err(Error::Invalid(_))
    ));
  }
  let mut bytes = zip_bytes(&[("bundle-set.json", b"x")]);
  let central = signature(&bytes, b"PK\x01\x02");
  bytes[central + 5] = 3; // Unix platform, symbolic link in external attributes.
  bytes[central + 38..central + 42].copy_from_slice(&(0o120_777_u32 << 16).to_le_bytes());
  assert!(matches!(extract(&bytes), Err(Error::Invalid(_))));
  let mut bytes = zip_bytes(&[("bundle-set.json", b"x")]);
  let central = signature(&bytes, b"PK\x01\x02");
  bytes[central + 8] |= 1; // Encrypted entry flag.
  assert!(matches!(extract(&bytes), Err(Error::Invalid(_))));
}

fn signature(bytes: &[u8], marker: &[u8]) -> usize {
  bytes
    .windows(marker.len())
    .position(|window| window == marker)
    .unwrap()
}

#[test]
fn duplicate_names_and_excessive_entries_are_rejected_before_extraction() {
  let mut bytes = zip_bytes(&[("aaa", b"a"), ("bbb", b"b")]);
  for start in 0..bytes.len() - 2 {
    if &bytes[start..start + 3] == b"bbb" {
      bytes[start..start + 3].copy_from_slice(b"aaa");
    }
  }
  assert!(matches!(extract(&bytes), Err(Error::Invalid(_))));
  let names: Vec<_> = (0..10).map(|index| format!("file-{index}")).collect();
  let entries: Vec<_> = names
    .iter()
    .map(|name| (name.as_str(), b"x".as_slice()))
    .collect();
  assert!(matches!(
    extract(&zip_bytes(&entries)),
    Err(Error::Invalid(_))
  ));
}

#[test]
fn decompression_manifest_and_target_sizes_are_bounded() {
  let oversized = vec![b' '; usize::try_from(MAX_MANIFEST_BYTES).unwrap() + 1];
  assert!(matches!(
    extract(&zip_bytes(&[("bundle-set.json", &oversized)])),
    Err(Error::Invalid(_))
  ));
  let mut bytes = fixture(b"archive");
  let first = signature(&bytes, b"PK\x01\x02");
  let second = first + 4 + signature(&bytes[first + 4..], b"PK\x01\x02");
  bytes[second + 24..second + 28]
    .copy_from_slice(&u32::try_from(MAX_ARCHIVE_BYTES + 1).unwrap().to_le_bytes());
  assert!(matches!(extract(&bytes), Err(Error::Invalid(_))));
}

#[test]
fn missing_target_checksum_or_revision_never_passes_extraction() {
  let manifest = serde_json::to_vec(&bundle_manifest(b"trusted")).unwrap();
  assert!(matches!(
    extract(&zip_bytes(&[("bundle-set.json", &manifest)])),
    Err(Error::Invalid(_))
  ));
  let name = format!("ctl-agent-bundle-0.1.0-{TARGET}.tar.gz");
  assert!(matches!(
    extract(&zip_bytes(&[
      ("bundle-set.json", &manifest),
      (&name, b"changed")
    ])),
    Err(Error::Invalid(_))
  ));
  let mut manifest = bundle_manifest(b"archive");
  manifest["git_revision"] = json!("a".repeat(40));
  let manifest = serde_json::to_vec(&manifest).unwrap();
  assert!(matches!(
    extract(&zip_bytes(&[
      ("bundle-set.json", &manifest),
      (&name, b"archive")
    ])),
    Err(Error::Stale(_))
  ));
}

#[cfg(unix)]
struct StreamGh {
  directory: TemporaryDirectory,
  program: std::path::PathBuf,
}

#[cfg(unix)]
impl StreamGh {
  fn new(body: &str) -> Self {
    let directory = TemporaryDirectory::new().unwrap();
    let program = gh_fixture(&directory.0, &format!("set -eu\n{body}\n"));
    Self { directory, program }
  }
}

#[cfg(unix)]
#[tokio::test]
async fn byte_progress_permits_a_healthy_download_beyond_five_minutes() {
  use std::sync::atomic::{AtomicU64, Ordering};
  let fake = StreamGh::new("for n in 1 2 3 4 5 6 7 8; do printf x; sleep 0.02; done");
  let destination = fake.directory.0.join("payload");
  let base = Instant::now();
  let seconds = AtomicU64::new(0);
  let mut received = 0;
  stream_command(
    fake.program.as_os_str(),
    "fixed-endpoint",
    &destination,
    8,
    || base + Duration::from_secs(seconds.load(Ordering::SeqCst)),
    Duration::from_millis(1),
    |progress| {
      if progress.transferred_bytes > received {
        received = progress.transferred_bytes;
        seconds.fetch_add(120, Ordering::SeqCst);
      }
    },
  )
  .await
  .unwrap();
  assert!(seconds.load(Ordering::SeqCst) > 300);
  assert_eq!(std::fs::read(destination).unwrap(), b"xxxxxxxx");
}

#[cfg(unix)]
#[tokio::test]
async fn idle_streams_and_processes_hanging_after_stdout_closes_are_killed() {
  use std::sync::atomic::{AtomicU64, Ordering};
  for (body, phase, expected) in [
    (
      "exec sleep 30",
      RemoteInstallPhase::Transferring,
      "Transfer stalled",
    ),
    (
      "printf x; exec 1>&-; exec sleep 30",
      RemoteInstallPhase::Checking,
      "component verification",
    ),
  ] {
    let fake = StreamGh::new(body);
    let base = Instant::now();
    let seconds = AtomicU64::new(0);
    let error = tokio::time::timeout(
      Duration::from_secs(5),
      stream_command(
        fake.program.as_os_str(),
        "fixed-endpoint",
        &fake.directory.0.join("payload"),
        1,
        || base + Duration::from_secs(seconds.load(Ordering::SeqCst)),
        Duration::from_millis(1),
        |progress| {
          // Reach the phase under test before advancing its idle deadline.
          if progress.phase == phase {
            seconds.fetch_add(31, Ordering::SeqCst);
          }
        },
      ),
    )
    .await
    .expect("GitHub command did not reach its idle deadline")
    .unwrap_err();
    assert!(error.to_string().contains(expected), "{error}");
  }
}

#[cfg(unix)]
#[tokio::test]
async fn streamed_size_limits_truncation_and_failure_details_are_preserved() {
  for (body, total, expected) in [
    ("printf xx", 1, "declared size"),
    ("printf x", 2, "truncated"),
    (
      "printf 'authentication required' >&2; exit 1",
      1,
      "authentication required",
    ),
  ] {
    let fake = StreamGh::new(body);
    let error = stream_command(
      fake.program.as_os_str(),
      "fixed-endpoint",
      &fake.directory.0.join("payload"),
      total,
      Instant::now,
      Duration::from_millis(1),
      |_| {},
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains(expected), "{error}");
  }
}

#[cfg(unix)]
#[tokio::test]
async fn private_staging_is_cleaned_after_success_or_failed_validation() {
  for trusted in [true, false] {
    let manifest = serde_json::to_vec(&bundle_manifest(b"archive")).unwrap();
    let name = format!("ctl-agent-bundle-0.1.0-{TARGET}.tar.gz");
    let payload = if trusted {
      b"archive".as_slice()
    } else {
      b"changed".as_slice()
    };
    let zip = zip_bytes(&[("bundle-set.json", &manifest), (&name, payload)]);
    let fake = StreamGh::new("exec cat \"$(dirname \"$0\")/fixture.zip\"");
    std::fs::write(fake.directory.0.join("fixture.zip"), &zip).unwrap();
    let directory = TemporaryDirectory::new().unwrap();
    let path = directory.0.clone();
    let artifact = Artifact {
      id: 202,
      name: ARTIFACT_NAME.into(),
      size_in_bytes: zip.len() as u64,
      expired: false,
    };
    let result = download_selected(
      fake.program.as_os_str(),
      artifact,
      directory,
      TARGET,
      &build(),
    )
    .await;
    assert_eq!(result.is_ok(), trusted);
    assert!(!path.exists());
  }
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_a_download_removes_its_private_staging() {
  let fake = StreamGh::new("exec sleep 30");
  let directory = TemporaryDirectory::new().unwrap();
  let path = directory.0.clone();
  let program = fake.program.clone();
  let artifact = Artifact {
    id: 202,
    name: ARTIFACT_NAME.into(),
    size_in_bytes: 42,
    expired: false,
  };
  let task = tokio::spawn(async move {
    download_selected(program.as_os_str(), artifact, directory, TARGET, &build()).await
  });
  tokio::time::timeout(Duration::from_secs(2), async {
    while !path.join(".download.zip").exists() {
      tokio::time::sleep(Duration::from_millis(1)).await;
    }
  })
  .await
  .unwrap();
  task.abort();
  assert!(task.await.unwrap_err().is_cancelled());
  assert!(!path.exists());
}
