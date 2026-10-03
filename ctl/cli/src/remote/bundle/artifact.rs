//! Existing Actions artifacts, streamed with an idle deadline and extracted
//! through the same version/revision/checksum policy as packaged resources.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use ctl_client::remote_bundle::{self, BundleSet, Error, VerifiedBundle};
use ctl_client::{RemoteInstallPhase, RemoteInstallProgress, RemoteInstallWatchdog};
use ctl_core::component::ComponentBuildInfo;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::watch;

use super::{LIST_TIMEOUT, TemporaryDirectory, read_capped, run_gh, spawn_gh};

const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ZIP_BYTES: u64 = 4 * MAX_ARCHIVE_BYTES + 256 * 1024;
const MAX_UNPACKED_BYTES: u64 = 4 * MAX_ARCHIVE_BYTES + MAX_MANIFEST_BYTES + 4 * 1024;
const ARTIFACT_NAME: &str = "ctl-agent-bundle-set";
const METADATA_FILTER: &str =
  "{total_count,artifacts:[.artifacts[]|{id,name,size_in_bytes,expired}]}";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactList {
  total_count: usize,
  artifacts: Vec<Artifact>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
  id: u64,
  name: String,
  size_in_bytes: u64,
  expired: bool,
}

fn select_artifact(bytes: &[u8]) -> Result<Artifact, Error> {
  let list: ArtifactList =
    serde_json::from_slice(bytes).map_err(|_| invalid("invalid GitHub artifact metadata"))?;
  if list.total_count > 100
    || list.total_count != list.artifacts.len()
    || list
      .artifacts
      .iter()
      .any(|artifact| artifact.id == 0 || artifact.name.len() > 256)
  {
    return Err(invalid("incomplete or invalid GitHub artifact listing"));
  }
  let mut matches = list
    .artifacts
    .into_iter()
    .filter(|artifact| artifact.name == ARTIFACT_NAME && !artifact.expired);
  let selected = matches.next().ok_or_else(|| {
    Error::NotAvailable("the matching workflow has no unexpired remote bundle-set artifact".into())
  })?;
  if matches.next().is_some()
    || selected.size_in_bytes == 0
    || selected.size_in_bytes > MAX_ZIP_BYTES
  {
    return Err(invalid("duplicate or oversized remote bundle-set artifact"));
  }
  Ok(selected)
}

pub(super) async fn download(
  program: &OsStr,
  run: u64,
  target: &str,
  expected: &ComponentBuildInfo,
) -> Result<VerifiedBundle, Error> {
  let endpoint = format!("repos/tokn-ai/ctl/actions/runs/{run}/artifacts?per_page=100");
  let bytes = run_gh(
    program,
    &[
      OsStr::new("api"),
      OsStr::new("--hostname"),
      OsStr::new("github.com"),
      OsStr::new("--method"),
      OsStr::new("GET"),
      OsStr::new(&endpoint),
      OsStr::new("--jq"),
      OsStr::new(METADATA_FILTER),
    ],
    LIST_TIMEOUT,
  )
  .await?;
  let artifact = select_artifact(&bytes)?;
  let directory = TemporaryDirectory::new()?;
  download_selected(program, artifact, directory, target, expected).await
}

async fn download_selected(
  program: &OsStr,
  artifact: Artifact,
  directory: TemporaryDirectory,
  target: &str,
  expected: &ComponentBuildInfo,
) -> Result<VerifiedBundle, Error> {
  let zip_path = directory.0.join(".download.zip");
  let endpoint = format!("repos/tokn-ai/ctl/actions/artifacts/{}/zip", artifact.id);
  let mut display = DownloadDisplay::default();
  stream_command(
    program,
    &endpoint,
    &zip_path,
    artifact.size_in_bytes,
    Instant::now,
    Duration::from_millis(500),
    |progress| display.show(progress),
  )
  .await?;
  display.finish();
  eprintln!("ctl: Verifying the downloaded CI component bundle...");
  let target = target.to_owned();
  let expected = expected.clone();
  tokio::task::spawn_blocking(move || {
    // Move the cleanup guard into the worker: cancellation must not remove its
    // files while it is still reading or extracting a bounded archive.
    let cleanup = directory;
    extract_verified(&zip_path, &cleanup.0, &target, &expected)
  })
  .await
  .map_err(|error| Error::Download(error.to_string()))?
}

async fn stream_command(
  program: &OsStr,
  endpoint: &str,
  destination: &Path,
  total_bytes: u64,
  now: impl Fn() -> Instant,
  tick: Duration,
  mut on_progress: impl FnMut(&RemoteInstallProgress),
) -> Result<(), Error> {
  let (mut child, stdout, stderr) = spawn_gh(
    program,
    &[
      OsStr::new("api"),
      OsStr::new("--hostname"),
      OsStr::new("github.com"),
      OsStr::new("--method"),
      OsStr::new("GET"),
      OsStr::new(endpoint),
    ],
  )?;
  let initial = RemoteInstallProgress {
    phase: RemoteInstallPhase::Transferring,
    file_name: Some("ctl-agent-bundle-set.zip".into()),
    total_bytes,
    ..RemoteInstallProgress::default()
  };
  let (updates, mut progress_updates) = watch::channel(initial);
  let result = {
    let operation = async {
      let (received, stderr, status) = tokio::try_join!(
        receive(stdout, destination, total_bytes, &updates),
        async { read_capped(stderr).await.map_err(Error::Io) },
        async { child.wait().await.map_err(Error::Io) },
      )?;
      if !status.success() {
        return Err(Error::Download(format!(
          "GitHub artifact download failed ({status}): {}",
          crate::table::text(&String::from_utf8_lossy(&stderr))
        )));
      }
      if received != total_bytes {
        return Err(invalid("truncated CI artifact ZIP download"));
      }
      Ok(())
    };
    tokio::pin!(operation);
    let mut watchdog = RemoteInstallWatchdog::new(now());
    let mut interval = tokio::time::interval(tick);
    loop {
      tokio::select! {
        result = &mut operation => break result,
        _ = interval.tick() => {},
        changed = progress_updates.changed() => { if changed.is_err() { break operation.await; } }
      }
      match watchdog.observe(progress_updates.borrow().clone(), now(), false) {
        Ok(progress) => on_progress(&progress),
        Err(error) => {
          break Err(Error::Download(format!(
            "CI artifact download stalled: {error}"
          )));
        }
      }
    }
  };
  if result.is_err() {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
  }
  result
}

async fn receive(
  mut stdout: impl AsyncRead + Unpin,
  destination: &Path,
  total_bytes: u64,
  updates: &watch::Sender<RemoteInstallProgress>,
) -> Result<u64, Error> {
  if total_bytes == 0 || total_bytes > MAX_ZIP_BYTES {
    return Err(invalid("invalid CI artifact download size"));
  }
  let mut file = tokio::fs::File::options()
    .write(true)
    .create_new(true)
    .open(destination)
    .await?;
  let mut received = 0_u64;
  let mut buffer = vec![0; 64 * 1024];
  loop {
    let count = stdout.read(&mut buffer).await?;
    if count == 0 {
      break;
    }
    let next = received.saturating_add(count as u64);
    if next > total_bytes || next > MAX_ZIP_BYTES {
      return Err(invalid(
        "CI artifact ZIP download exceeds its declared size limit",
      ));
    }
    file.write_all(&buffer[..count]).await?;
    received = next;
    updates.send_modify(|progress| progress.transferred_bytes = received);
  }
  // Keep watching completion after stdout closes: gh can still be stuck with
  // stderr open or its process alive, even after the complete ZIP arrived.
  updates.send_modify(|progress| progress.phase = RemoteInstallPhase::Checking);
  file.flush().await?;
  Ok(received)
}

fn extract_verified(
  path: &Path,
  destination: &Path,
  target: &str,
  expected: &ComponentBuildInfo,
) -> Result<VerifiedBundle, Error> {
  let mut file = File::open(path)?;
  let entries = central_entry_count(&mut file)?;
  let mut archive = zip::ZipArchive::new(file).map_err(|error| zip_error(&error))?;
  if archive.offset() != 0 || archive.len() != entries {
    return Err(invalid(
      "CI artifact ZIP contains duplicate names or an unexpected prefix",
    ));
  }
  for index in 0..archive.len() {
    let entry = archive.by_index(index).map_err(|error| zip_error(&error))?;
    validate_regular_entry(&entry)?;
  }
  let manifest = {
    let entry = archive
      .by_name("bundle-set.json")
      .map_err(|error| zip_error(&error))?;
    if entry.size() > MAX_MANIFEST_BYTES {
      return Err(invalid("CI artifact manifest is oversized"));
    }
    let mut bytes = Vec::new();
    entry.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
      return Err(invalid("CI artifact manifest is oversized"));
    }
    bytes
  };
  let bundle_set = BundleSet::parse(&manifest, &expected.version)?;
  bundle_set.verify_revision(expected)?;
  let target_archive = bundle_set.archive_name(target)?.to_owned();
  let mut allowed = BTreeSet::from(["bundle-set.json".to_owned()]);
  for name in bundle_set.archive_names() {
    allowed.insert(name.to_owned());
    allowed.insert(format!("{name}.sha256"));
  }
  let mut unpacked = 0_u64;
  for index in 0..archive.len() {
    let entry = archive.by_index(index).map_err(|error| zip_error(&error))?;
    if !allowed.contains(entry.name()) {
      return Err(invalid("CI artifact contains an unexpected file"));
    }
    let maximum = if entry.name() == "bundle-set.json" {
      MAX_MANIFEST_BYTES
    } else if entry.name().ends_with(".sha256") {
      1024
    } else {
      MAX_ARCHIVE_BYTES
    };
    unpacked = unpacked.saturating_add(entry.size());
    if entry.size() > maximum || unpacked > MAX_UNPACKED_BYTES {
      return Err(invalid("CI artifact exceeds its unpacked size limit"));
    }
  }
  write_new(&destination.join("bundle-set.json"), &manifest)?;
  let entry = archive
    .by_name(&target_archive)
    .map_err(|error| zip_error(&error))?;
  let expected_size = entry.size();
  let mut output = File::options()
    .write(true)
    .create_new(true)
    .open(destination.join(&target_archive))?;
  let copied = io::copy(&mut entry.take(MAX_ARCHIVE_BYTES + 1), &mut output)?;
  if copied > MAX_ARCHIVE_BYTES || copied != expected_size {
    return Err(invalid(
      "CI artifact target archive exceeds its size or is truncated",
    ));
  }
  remote_bundle::read_verified_bundle(&[destination.to_owned()], target, expected)?
    .ok_or_else(|| invalid("CI artifact contains no verified bundle"))
}

fn validate_regular_entry<R: io::Read>(entry: &zip::read::ZipFile<'_, R>) -> Result<(), Error> {
  let name = entry.name();
  if name.is_empty()
    || name.len() > 256
    || !name.is_ascii()
    || name.contains(['/', '\\', ':'])
    || matches!(name, "." | "..")
    || name.chars().any(char::is_control)
    || entry.enclosed_name().is_none()
    || !entry.is_file()
    || entry.encrypted()
    || entry
      .unix_mode()
      .is_some_and(|mode| mode & 0o170_000 != 0 && mode & 0o170_000 != 0o100_000)
  {
    return Err(invalid(
      "CI artifact ZIP contains unsafe paths or special files",
    ));
  }
  Ok(())
}

/// `ZipArchive` deduplicates names while building its index. Preflight the small
/// ordinary ZIP central directory so its declared count can detect duplicates
/// and bound the reader's metadata allocation before `ZipArchive` is constructed.
fn central_entry_count(file: &mut File) -> Result<usize, Error> {
  let size = file.metadata()?.len();
  if !(22..=MAX_ZIP_BYTES).contains(&size) {
    return Err(invalid("invalid CI artifact ZIP size"));
  }
  let tail_size = size.min(65_535 + 22);
  file.seek(SeekFrom::Start(size - tail_size))?;
  let mut tail = Vec::new();
  file.read_to_end(&mut tail)?;
  let start = (0..=tail.len() - 22)
    .rev()
    .find(|start| {
      tail[*start..].starts_with(b"PK\x05\x06")
        && usize::from(u16::from_le_bytes([tail[*start + 20], tail[*start + 21]])) + *start + 22
          == tail.len()
    })
    .ok_or_else(|| invalid("CI artifact ZIP has no complete central directory"))?;
  let eocd = &tail[start..];
  let count = usize::from(u16_at(eocd, 10));
  let directory_size = u64::from(u32_at(eocd, 12));
  let offset = u64::from(u32_at(eocd, 16));
  let end = size - tail_size + start as u64;
  if count == 0
    || count > 9
    || u16_at(eocd, 4) != 0
    || u16_at(eocd, 6) != 0
    || usize::from(u16_at(eocd, 8)) != count
    || offset.checked_add(directory_size) != Some(end)
  {
    return Err(invalid(
      "unsupported or oversized CI artifact ZIP central directory",
    ));
  }
  file.seek(SeekFrom::Start(offset))?;
  for _ in 0..count {
    let mut header = [0; 46];
    file.read_exact(&mut header)?;
    if !header.starts_with(b"PK\x01\x02") {
      return Err(invalid("invalid CI artifact ZIP central entry"));
    }
    let extra = u64::from(u16_at(&header, 28))
      + u64::from(u16_at(&header, 30))
      + u64::from(u16_at(&header, 32));
    let next = file
      .stream_position()?
      .checked_add(extra)
      .ok_or_else(|| invalid("invalid CI artifact ZIP offsets"))?;
    if next > end {
      return Err(invalid(
        "CI artifact ZIP entry exceeds its central directory",
      ));
    }
    file.seek(SeekFrom::Start(next))?;
  }
  if file.stream_position()? != end {
    return Err(invalid(
      "CI artifact ZIP central directory has extra entries",
    ));
  }
  file.rewind()?;
  Ok(count)
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
  u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
  u32::from_le_bytes([
    bytes[offset],
    bytes[offset + 1],
    bytes[offset + 2],
    bytes[offset + 3],
  ])
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Error> {
  File::options()
    .write(true)
    .create_new(true)
    .open(path)?
    .write_all(bytes)?;
  Ok(())
}

fn invalid(message: &str) -> Error {
  Error::Invalid(message.into())
}
fn zip_error(error: &zip::result::ZipError) -> Error {
  invalid(&format!("invalid CI artifact ZIP: {error}"))
}

#[derive(Default)]
struct DownloadDisplay {
  active: bool,
}

impl DownloadDisplay {
  fn show(&mut self, progress: &RemoteInstallProgress) {
    let filled =
      usize::try_from(progress.transferred_bytes.saturating_mul(20) / progress.total_bytes.max(1))
        .unwrap_or(20)
        .min(20);
    eprint!(
      "\r\x1b[2Kctl: Downloading CI bundle [{}{}] {}/{} bytes, {} B/s",
      "=".repeat(filled),
      " ".repeat(20 - filled),
      progress.transferred_bytes,
      progress.total_bytes,
      progress.bytes_per_second
    );
    let _ = io::stderr().flush();
    self.active = true;
  }
  fn finish(&mut self) {
    if self.active {
      eprintln!();
      self.active = false;
    }
  }
}

impl Drop for DownloadDisplay {
  fn drop(&mut self) {
    self.finish();
  }
}

#[cfg(test)]
#[path = "artifact/tests.rs"]
mod tests;
