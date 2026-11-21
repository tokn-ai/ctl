use super::{
  Asset, DesktopRelease, DesktopSigning, DesktopTarget, MAX_PACKAGE_BYTES, PackageSource, invalid,
  open_package, parse_manifest, select_asset,
};
use crate::remote_bundle::{Error, MAX_BUNDLE_SET_BYTES, github, pr, published, read_bounded_file};
use ctl_core::private_fs;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

const REPOSITORY: &str = "tokn-ai/ctl";
const PAGE_SIZE: usize = 100;

pub(super) async fn resolve(
  publication: published::PublishedRelease,
  target: DesktopTarget,
  staging: &Path,
) -> io::Result<DesktopRelease> {
  let github = github::Github::new().map_err(io::Error::other)?;
  let (run_id, run_attempt, artifact_name) = artifact(&github, &publication, target).await?;
  let directory = DownloadedArtifact::new(staging)?;
  github::download_artifact(run_id, &artifact_name, &directory.0)
    .await
    .map_err(io::Error::other)?;
  let current: WorkflowRun = github
    .get(&format!("actions/runs/{run_id}"), &[])
    .await
    .map_err(io::Error::other)?;
  if !current.matches(run_id, run_attempt, &publication.git_revision) {
    return Err(invalid(
      "Nightly workflow changed while downloading its desktop artifact; retry",
    ));
  }
  let (signing, asset) = artifact_manifest(
    &directory.0,
    target,
    &publication.bundle_id,
    &publication.git_revision,
  )?;
  Ok(DesktopRelease {
    app_version: publication.app_version,
    bundle_id: publication.bundle_id,
    publication_id: Some(publication.publication_id),
    asset,
    signing,
    source: PackageSource::Workflow(directory),
  })
}

pub(super) async fn available(
  publication: &published::PublishedRelease,
  target: DesktopTarget,
) -> io::Result<bool> {
  let github = github::Github::new().map_err(io::Error::other)?;
  match artifact(&github, publication, target).await {
    Ok(_) => Ok(true),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error),
  }
}

async fn artifact(
  github: &github::Github,
  publication: &published::PublishedRelease,
  target: DesktopTarget,
) -> io::Result<(u64, u64, String)> {
  let (run_id, run_attempt) = publication_run(publication)?;
  let run: WorkflowRun = github
    .get(&format!("actions/runs/{run_id}"), &[])
    .await
    .map_err(io::Error::other)?;
  if !run.matches(run_id, run_attempt, &publication.git_revision) {
    return Err(unavailable(&publication.bundle_id, target));
  }
  let artifact_name = format!("ctmux-desktop-{}", target.triple());
  let mut page = 1;
  let mut selected = false;
  loop {
    let response: ArtifactPage = github
      .get(
        &format!("actions/runs/{run_id}/artifacts"),
        &[
          ("per_page", &PAGE_SIZE.to_string()),
          ("page", &page.to_string()),
        ],
      )
      .await
      .map_err(io::Error::other)?;
    let has_more =
      response.artifacts.len() == PAGE_SIZE && page < response.total_count.div_ceil(PAGE_SIZE);
    for artifact in response.artifacts {
      if artifact.matches(&artifact_name, &run) {
        if selected {
          return Err(invalid("Nightly workflow has duplicate desktop artifacts"));
        }
        selected = true;
      }
    }
    if !has_more {
      break;
    }
    page += 1;
  }
  if !selected {
    return Err(unavailable(&publication.bundle_id, target));
  }
  Ok((run_id, run_attempt, artifact_name))
}

pub(super) async fn pull_request(
  number: u64,
  target: DesktopTarget,
  staging: &Path,
) -> io::Result<DesktopRelease> {
  let name = format!("ctmux-desktop-{}", target.triple());
  let directory = DownloadedArtifact::new(staging)?;
  let revision = pr::download_named_artifact(number, &name, &directory.0)
    .await
    .map_err(io::Error::other)?;
  let (version, bundle_id) = artifact_identity(&directory.0, target)?;
  if bundle_id != format!("{version}-dev.{}", &revision[..12]) {
    return Err(invalid(
      "desktop package does not match the current PR head",
    ));
  }
  let (signing, asset) = artifact_manifest(&directory.0, target, &bundle_id, &revision)?;
  Ok(DesktopRelease {
    app_version: version,
    bundle_id,
    publication_id: None,
    asset,
    signing,
    source: PackageSource::Workflow(directory),
  })
}

fn artifact_identity(directory: &Path, target: DesktopTarget) -> io::Result<(String, String)> {
  let mut suffixes = vec![""];
  if target.is_macos() {
    suffixes.push("-unsigned");
  }
  for suffix in suffixes {
    let path = directory.join(format!("desktop-{}{suffix}.json", target.triple()));
    let bytes = match read_bounded_file(&path, MAX_BUNDLE_SET_BYTES) {
      Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => continue,
      result => result.map_err(io::Error::other)?,
    };
    let manifest: super::Manifest = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let (version, _) = manifest
      .bundle_id
      .split_once("-dev.")
      .ok_or_else(|| invalid("desktop PR artifact has an invalid build identity"))?;
    if !crate::remote_bundle::safe_id(version) {
      return Err(invalid("desktop PR artifact has an invalid app version"));
    }
    return Ok((version.to_owned(), manifest.bundle_id));
  }
  Err(invalid(
    "desktop PR artifact does not contain a package manifest",
  ))
}

fn artifact_manifest(
  directory: &Path,
  target: DesktopTarget,
  bundle_id: &str,
  git_revision: &str,
) -> io::Result<(DesktopSigning, Asset)> {
  let canonical = directory.join(format!("desktop-{}.json", target.triple()));
  let (bytes, suffix) = match read_bounded_file(&canonical, MAX_BUNDLE_SET_BYTES) {
    Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound && target.is_macos() => {
      let unsigned = directory.join(format!("desktop-{}-unsigned.json", target.triple()));
      (
        read_bounded_file(&unsigned, MAX_BUNDLE_SET_BYTES).map_err(io::Error::other)?,
        "-unsigned",
      )
    }
    result => (result.map_err(io::Error::other)?, ""),
  };
  let manifest = parse_manifest(&bytes, target, bundle_id, git_revision)?;
  let signing = manifest.signing_mode;
  if !target.signing_options().contains(&signing)
    || (!suffix.is_empty() && signing != DesktopSigning::Unsigned)
  {
    return Err(invalid("unexpected Nightly desktop signing mode"));
  }
  let asset = select_asset(manifest.assets, target, bundle_id, suffix)?;
  Ok((signing, asset))
}

pub(super) struct DownloadedArtifact(PathBuf);

impl DownloadedArtifact {
  fn new(staging: &Path) -> io::Result<Self> {
    let path = staging.join(format!("workflow-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&path)?;
    Ok(Self(path))
  }

  pub(super) fn copy_verified(&self, asset: &Asset, destination: &mut File) -> io::Result<()> {
    let mut source = open_package(&self.0.join(&asset.name))?.take(MAX_PACKAGE_BYTES + 1);
    let mut buffer = vec![0; 64 * 1024];
    let mut size = 0;
    let mut digest = Sha256::new();
    loop {
      let count = source.read(&mut buffer)?;
      if count == 0 {
        break;
      }
      size += count as u64;
      if size > MAX_PACKAGE_BYTES {
        return Err(invalid("desktop package exceeds its download size limit"));
      }
      digest.update(&buffer[..count]);
      destination.write_all(&buffer[..count])?;
    }
    if size == 0 || !asset.matches(digest) {
      return Err(invalid("desktop package SHA-256 checksum mismatch"));
    }
    Ok(())
  }
}

impl Drop for DownloadedArtifact {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn publication_run(publication: &published::PublishedRelease) -> io::Result<(u64, u64)> {
  let (run_id, run_attempt) = publication
    .publication_id
    .strip_prefix(&format!("{}-", publication.bundle_id))
    .and_then(|suffix| suffix.split_once('-'))
    .ok_or_else(|| invalid("Nightly publication has no workflow identity"))?;
  let run_id = run_id
    .parse()
    .ok()
    .filter(|id| *id != 0)
    .ok_or_else(|| invalid("invalid Nightly workflow run ID"))?;
  let run_attempt = run_attempt
    .parse()
    .ok()
    .filter(|attempt| *attempt != 0)
    .ok_or_else(|| invalid("invalid Nightly workflow attempt"))?;
  Ok((run_id, run_attempt))
}

#[derive(Deserialize)]
struct WorkflowRun {
  id: u64,
  path: String,
  event: String,
  run_attempt: u64,
  repository: Repository,
  head_repository: Option<Repository>,
  head_branch: String,
  head_sha: String,
}

impl WorkflowRun {
  fn matches(&self, run_id: u64, run_attempt: u64, revision: &str) -> bool {
    self.id == run_id
      && self.run_attempt == run_attempt
      && self.path == ".github/workflows/bundles.yml"
      && matches!(self.event.as_str(), "push" | "workflow_dispatch")
      && self.head_branch == "main"
      && self.head_sha == revision
      && self.repository.id != 0
      && self.repository.full_name == REPOSITORY
      && self.head_repository.as_ref().is_some_and(|repository| {
        repository.id == self.repository.id && repository.full_name == REPOSITORY
      })
  }
}

#[derive(Deserialize)]
struct Repository {
  id: u64,
  full_name: String,
}

#[derive(Deserialize)]
struct ArtifactPage {
  total_count: usize,
  artifacts: Vec<Artifact>,
}

#[derive(Deserialize)]
struct Artifact {
  id: u64,
  name: String,
  expired: bool,
  workflow_run: Option<ArtifactRun>,
}

impl Artifact {
  fn matches(&self, name: &str, workflow: &WorkflowRun) -> bool {
    self.id != 0
      && !self.expired
      && self.name == name
      && self.workflow_run.as_ref().is_some_and(|run| {
        run.id == workflow.id
          && run.repository_id == workflow.repository.id
          && run.head_repository_id == workflow.repository.id
          && run.head_branch == workflow.head_branch
          && run.head_sha == workflow.head_sha
      })
  }
}

#[derive(Deserialize)]
struct ArtifactRun {
  id: u64,
  repository_id: u64,
  head_repository_id: u64,
  head_branch: String,
  head_sha: String,
}

fn unavailable(bundle_id: &str, target: DesktopTarget) -> io::Error {
  io::Error::new(
    io::ErrorKind::NotFound,
    format!(
      "Nightly desktop package for {} is unavailable for {bundle_id}",
      target.triple()
    ),
  )
}

#[cfg(test)]
mod tests;
