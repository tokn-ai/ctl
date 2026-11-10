//! Temporary bundle builds for pull requests in the ctl repository.

use super::{
  BUNDLE_SET_FILE, BundleSet, Error, MAX_BUNDLE_SET_BYTES, download_error, read_bounded_file,
};
use reqwest::{Client, Url, header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

const REPOSITORY: &str = "tokn-ai/ctl";
const REPOSITORY_URL: &str = "https://github.com/tokn-ai/ctl";
const BUNDLE_WORKFLOW: &str = ".github/workflows/bundles.yml";
const ARTIFACT_NAME: &str = "ctl-agent-bundle-set";
const MAX_GITHUB_JSON_BYTES: usize = 4 * 1024 * 1024;
const PAGE_SIZE: usize = 100;

/// A recent pull request and its available build, if one matches the current head.
#[derive(Debug, Serialize)]
pub struct PullRequestBuild {
  pub number: u64,
  pub title: String,
  pub head_sha: String,
  pub url: String,
  pub status: PullRequestBuildStatus,
  pub build: Option<PullRequestArtifact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestBuildStatus {
  Built,
  Outdated,
  NoBuild,
}

#[derive(Debug, Serialize)]
pub struct PullRequestArtifact {
  pub run_id: u64,
  pub artifact_id: u64,
  pub created_at: String,
  pub expires_at: String,
  pub url: String,
}

/// A private temporary directory holding the selected pull request build.
pub struct DownloadedPrBundle(PathBuf);

impl DownloadedPrBundle {
  fn new() -> io::Result<Self> {
    let directory = std::env::temp_dir().join(format!("ctlup-pr-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
    Ok(Self(directory))
  }

  #[must_use]
  pub fn directory(&self) -> &Path {
    &self.0
  }
}

impl Drop for DownloadedPrBundle {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

/// List the ten most recently updated pull requests and their build status.
/// # Errors
/// Reports GitHub API failures or invalid response metadata.
pub async fn list() -> Result<Vec<PullRequestBuild>, Error> {
  let github = Github::new()?;
  let (pulls, artifacts) = tokio::try_join!(github.recent_pulls(), github.artifacts())?;
  let runs = github.candidate_runs(&pulls, &artifacts, &[]).await?;
  pulls
    .into_iter()
    .map(|pull| pull.resolve(&artifacts, &runs))
    .collect()
}

/// View a pull request's build status using public GitHub metadata.
/// # Errors
/// Reports missing pull requests, API failures, or invalid response metadata.
pub async fn view(number: u64) -> Result<PullRequestBuild, Error> {
  Github::new()?.view(number).await
}

/// Download the current build through the authenticated GitHub CLI.
/// # Errors
/// Reports missing builds, download failures, or a bundle that does not match the PR head.
pub async fn download(number: u64) -> Result<DownloadedPrBundle, Error> {
  let github = Github::new()?;
  let selection = github.view(number).await?;
  let build = selection.build.ok_or_else(|| {
    Error::NotAvailable(format!(
      "PR #{number} has no build for {}",
      selection.head_sha
    ))
  })?;
  let bundle = DownloadedPrBundle::new()?;
  let status = tokio::process::Command::new("gh")
    .args([
      "run",
      "download",
      &build.run_id.to_string(),
      "--repo",
      "github.com/tokn-ai/ctl",
    ])
    .args(["--name", ARTIFACT_NAME, "--dir"])
    .arg(bundle.directory())
    .env("GH_PROMPT_DISABLED", "1")
    .env("TMPDIR", bundle.directory())
    .kill_on_drop(true)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .status()
    .await
    .map_err(|error| {
      if error.kind() == io::ErrorKind::NotFound {
        Error::Download(
          "install the GitHub CLI and run `gh auth login` to download PR builds".into(),
        )
      } else {
        Error::Io(error)
      }
    })?;
  if !status.success() {
    return Err(Error::Download(format!(
      "GitHub CLI could not download the PR #{number} bundle from run {}; check `gh auth status`",
      build.run_id
    )));
  }
  let current = github.pull(number).await?;
  if current.head.sha != selection.head_sha {
    return Err(Error::Stale(format!(
      "PR #{number} changed while downloading its build"
    )));
  }
  let revision = selection.head_sha;
  tokio::task::spawn_blocking(move || {
    verify_download(bundle.directory(), &revision)?;
    Ok(bundle)
  })
  .await
  .map_err(io::Error::other)?
}

fn verify_download(directory: &Path, revision: &str) -> Result<(), Error> {
  let bytes = read_bounded_file(&directory.join(BUNDLE_SET_FILE), MAX_BUNDLE_SET_BYTES)?;
  let manifest = BundleSet::parse_intrinsic(&bytes)?;
  if manifest.git_revision != revision
    || manifest.bundle_id != format!("{}-dev.{}", manifest.app_version, &revision[..12])
  {
    return Err(Error::Stale(
      "downloaded bundle does not match the current PR build".into(),
    ));
  }
  // A supplied build directory can also describe a managed bundle. Allow only
  // publisher bundle files so installation always uses the manifest above.
  let mut expected = BTreeSet::from([BUNDLE_SET_FILE.to_owned()]);
  for target in super::SUPPORTED_TARGETS {
    let archive = manifest.archive_name(target)?;
    let updater_manifest = format!("ctlup-{target}.json");
    let metadata = match read_bounded_file(&directory.join(&updater_manifest), MAX_BUNDLE_SET_BYTES)
    {
      Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
        return Err(Error::NotAvailable(format!(
          "PR build is missing the ctlup updater for {target}; rebuild the PR with the current bundle workflow"
        )));
      }
      result => result?,
    };
    let updater = super::published::UpdaterInfo::parse(&metadata, target)?;
    if updater.app_version != manifest.app_version
      || updater.git_revision != manifest.git_revision
      || updater.bundle_id != manifest.bundle_id
    {
      return Err(Error::Invalid(format!(
        "PR bundle and updater differ for {target}"
      )));
    }
    expected.extend([
      archive.to_owned(),
      format!("{archive}.sha256"),
      format!("{}.sha256", updater.binary),
      updater.binary,
      updater_manifest,
    ]);
  }
  for entry in std::fs::read_dir(directory)? {
    let entry = entry?;
    if !entry.file_type()?.is_file()
      || !expected.remove(entry.file_name().to_string_lossy().as_ref())
    {
      return Err(Error::Invalid(
        "PR artifact contains an unexpected file".into(),
      ));
    }
  }
  if !expected.is_empty() {
    return Err(Error::Invalid("PR artifact is missing bundle files".into()));
  }
  Ok(())
}

struct Github(Client);

impl Github {
  fn new() -> Result<Self, Error> {
    let headers = header::HeaderMap::from_iter([
      (
        header::ACCEPT,
        header::HeaderValue::from_static("application/vnd.github+json"),
      ),
      (
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
      ),
    ]);
    Client::builder()
      .https_only(true)
      .redirect(reqwest::redirect::Policy::none())
      .connect_timeout(Duration::from_secs(15))
      .read_timeout(Duration::from_secs(30))
      .user_agent(concat!("ctlup/", env!("CARGO_PKG_VERSION")))
      .default_headers(headers)
      .build()
      .map(Self)
      .map_err(download_error)
  }

  async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T, Error> {
    let mut url = Url::parse(&format!("https://api.github.com/repos/{REPOSITORY}/{path}"))
      .map_err(|error| Error::Invalid(error.to_string()))?;
    if !query.is_empty() {
      url.query_pairs_mut().extend_pairs(query);
    }
    let bytes = super::download(&self.0, url.as_str(), MAX_GITHUB_JSON_BYTES).await?;
    serde_json::from_slice(&bytes)
      .map_err(|_| Error::Invalid("GitHub returned invalid PR build metadata".into()))
  }

  async fn recent_pulls(&self) -> Result<Vec<PullRequest>, Error> {
    self
      .get(
        "pulls",
        &[
          ("state", "all"),
          ("sort", "updated"),
          ("direction", "desc"),
          ("per_page", "10"),
        ],
      )
      .await
  }

  async fn pull(&self, number: u64) -> Result<PullRequest, Error> {
    let pull: PullRequest = self
      .get(&format!("pulls/{number}"), &[])
      .await
      .map_err(|error| {
        if matches!(error, Error::NotAvailable(_)) {
          Error::NotAvailable(format!("GitHub did not find PR #{number}"))
        } else {
          error
        }
      })?;
    if pull.number != number {
      return Err(Error::Invalid(
        "GitHub returned a different pull request".into(),
      ));
    }
    Ok(pull)
  }

  async fn artifacts(&self) -> Result<Vec<Artifact>, Error> {
    let mut artifacts = Vec::new();
    let mut page = 1;
    loop {
      let response: ArtifactPage = self
        .get(
          "actions/artifacts",
          &[
            ("name", ARTIFACT_NAME),
            ("per_page", &PAGE_SIZE.to_string()),
            ("page", &page.to_string()),
          ],
        )
        .await?;
      let has_more =
        response.artifacts.len() == PAGE_SIZE && page < response.total_count.div_ceil(PAGE_SIZE);
      artifacts.extend(
        response
          .artifacts
          .into_iter()
          .filter(|artifact| !artifact.expired),
      );
      if !has_more {
        break;
      }
      page += 1;
    }
    artifacts.sort_unstable_by(|left, right| {
      (right.created_at.as_str(), right.id).cmp(&(left.created_at.as_str(), left.id))
    });
    Ok(artifacts)
  }

  async fn candidate_runs(
    &self,
    pulls: &[PullRequest],
    artifacts: &[Artifact],
    filters: &[(&str, &str)],
  ) -> Result<BTreeMap<u64, WorkflowRun>, Error> {
    let mut candidates: BTreeSet<_> = artifacts
      .iter()
      .filter_map(|artifact| {
        pulls
          .iter()
          .find_map(|pull| artifact.matching_run(&pull.head))
          .map(|run| run.id)
      })
      .collect();
    let mut runs = BTreeMap::new();
    let mut page = 1;
    let page_size = PAGE_SIZE.to_string();
    while !candidates.is_empty() {
      let page_number = page.to_string();
      let mut query = vec![
        ("event", "workflow_dispatch"),
        ("per_page", &page_size),
        ("page", &page_number),
      ];
      query.extend_from_slice(filters);
      let response: WorkflowRunPage = self
        .get("actions/workflows/bundles.yml/runs", &query)
        .await?;
      let has_more = response.workflow_runs.len() == PAGE_SIZE
        && page < response.total_count.div_ceil(PAGE_SIZE);
      for run in response.workflow_runs {
        if candidates.remove(&run.id) {
          runs.insert(run.id, run);
        }
      }
      if !has_more {
        break;
      }
      page += 1;
    }
    Ok(runs)
  }

  async fn view(&self, number: u64) -> Result<PullRequestBuild, Error> {
    let (pull, artifacts) = tokio::try_join!(self.pull(number), self.artifacts())?;
    let runs = self
      .candidate_runs(
        std::slice::from_ref(&pull),
        &artifacts,
        &[("branch", &pull.head.branch)],
      )
      .await?;
    pull.resolve(&artifacts, &runs)
  }
}

#[derive(Deserialize)]
struct PullRequest {
  number: u64,
  title: String,
  head: PullHead,
}

impl PullRequest {
  fn resolve(
    self,
    artifacts: &[Artifact],
    runs: &BTreeMap<u64, WorkflowRun>,
  ) -> Result<PullRequestBuild, Error> {
    if self.head.sha.len() != 40 || !self.head.sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
      return Err(Error::Invalid(
        "GitHub returned an invalid PR head revision".into(),
      ));
    }
    let mut status = PullRequestBuildStatus::NoBuild;
    let mut build = None;
    for artifact in artifacts {
      let Some(reference) = artifact.matching_run(&self.head) else {
        continue;
      };
      let Some(run) = runs.get(&reference.id).filter(|run| run.matches(reference)) else {
        continue;
      };
      if run.head_sha != self.head.sha {
        status = PullRequestBuildStatus::Outdated;
        continue;
      }
      status = PullRequestBuildStatus::Built;
      build = Some(PullRequestArtifact {
        run_id: run.id,
        artifact_id: artifact.id,
        created_at: artifact.created_at.clone(),
        expires_at: artifact.expires_at.clone(),
        url: format!("{REPOSITORY_URL}/actions/runs/{}", run.id),
      });
      break;
    }
    Ok(PullRequestBuild {
      number: self.number,
      title: self.title,
      head_sha: self.head.sha,
      url: format!("{REPOSITORY_URL}/pull/{}", self.number),
      status,
      build,
    })
  }
}

#[derive(Deserialize)]
struct PullHead {
  sha: String,
  #[serde(rename = "ref")]
  branch: String,
  repo: Option<Repository>,
}

#[derive(Deserialize)]
struct Repository {
  id: u64,
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
  created_at: String,
  expires_at: String,
  workflow_run: Option<ArtifactRun>,
}

impl Artifact {
  fn matching_run(&self, head: &PullHead) -> Option<&ArtifactRun> {
    let run = self.workflow_run.as_ref()?;
    let repository = head.repo.as_ref()?;
    (!self.expired
      && self.name == ARTIFACT_NAME
      && self.id != 0
      && run.id != 0
      && run.head_repository_id == repository.id
      && run.head_branch == head.branch)
      .then_some(run)
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

#[derive(Deserialize)]
struct WorkflowRunPage {
  total_count: usize,
  workflow_runs: Vec<WorkflowRun>,
}

#[derive(Deserialize)]
struct WorkflowRun {
  id: u64,
  path: String,
  event: String,
  status: String,
  conclusion: Option<String>,
  repository: Repository,
  head_repository: Option<Repository>,
  head_branch: String,
  head_sha: String,
}

impl WorkflowRun {
  fn matches(&self, artifact: &ArtifactRun) -> bool {
    self.id == artifact.id
      && self.path == BUNDLE_WORKFLOW
      && self.event == "workflow_dispatch"
      && self.status == "completed"
      // Bundle assembly has already succeeded when the artifact exists. An
      // optional desktop job can still fail without invalidating that bundle.
      && matches!(self.conclusion.as_deref(), Some("success" | "failure"))
      && self.repository.id == artifact.repository_id
      && self.head_repository.as_ref().map(|repo| repo.id) == Some(artifact.head_repository_id)
      && (self.head_branch != "main" || self.repository.id != artifact.head_repository_id)
      && self.head_branch == artifact.head_branch
      && self.head_sha == artifact.head_sha
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn reports_outdated_builds_but_only_selects_a_verified_current_build() {
    let revision = "a".repeat(40);
    let older_revision = "b".repeat(40);
    let pull = || {
      serde_json::from_value::<PullRequest>(json!({
        "number": 155,
        "title": "Improve updates",
        "head": { "sha": revision, "ref": "updates", "repo": { "id": 42 } }
      }))
      .unwrap()
    };
    let artifact = |id, expired, repo_id, branch: &str, sha: &str| {
      serde_json::from_value::<Artifact>(json!({
        "id": id,
        "name": ARTIFACT_NAME,
        "expired": expired,
        "created_at": "2026-11-10T00:00:00Z",
        "expires_at": "2026-12-10T00:00:00Z",
        "workflow_run": {
          "id": id,
          "repository_id": 42,
          "head_repository_id": repo_id,
          "head_branch": branch,
          "head_sha": sha
        }
      }))
      .unwrap()
    };
    let run = |id, path: &str, conclusion: &str, sha: &str| {
      serde_json::from_value::<WorkflowRun>(json!({
        "id": id,
        "path": path,
        "event": "workflow_dispatch",
        "status": "completed",
        "conclusion": conclusion,
        "head_branch": "updates",
        "head_sha": sha,
        "repository": { "id": 42 },
        "head_repository": { "id": 42 }
      }))
      .unwrap()
    };
    let mut artifacts = vec![
      artifact(7, true, 42, "updates", &revision),
      artifact(6, false, 99, "updates", &revision),
      artifact(5, false, 42, "other", &revision),
      artifact(4, false, 42, "updates", &older_revision),
      artifact(3, false, 42, "updates", &revision),
      artifact(2, false, 42, "updates", &revision),
    ];
    let runs = BTreeMap::from([
      (8, run(8, BUNDLE_WORKFLOW, "success", &older_revision)),
      (7, run(7, BUNDLE_WORKFLOW, "success", &revision)),
      (6, run(6, BUNDLE_WORKFLOW, "success", &revision)),
      (5, run(5, BUNDLE_WORKFLOW, "success", &revision)),
      (4, run(4, BUNDLE_WORKFLOW, "success", &revision)),
      (3, run(3, ".github/workflows/ci.yml", "success", &revision)),
      (2, run(2, BUNDLE_WORKFLOW, "cancelled", &revision)),
      (1, run(1, BUNDLE_WORKFLOW, "failure", &revision)),
    ]);
    let missing = pull().resolve(&artifacts, &runs).unwrap();
    assert_eq!(missing.status, PullRequestBuildStatus::NoBuild);
    assert!(missing.build.is_none());

    artifacts.insert(0, artifact(8, false, 42, "updates", &older_revision));
    let outdated = pull().resolve(&artifacts, &runs).unwrap();
    assert_eq!(outdated.status, PullRequestBuildStatus::Outdated);
    assert!(outdated.build.is_none());

    artifacts.push(artifact(1, false, 42, "updates", &revision));
    let current = pull().resolve(&artifacts, &runs).unwrap();
    assert_eq!(current.status, PullRequestBuildStatus::Built);
    let build = current.build.unwrap();
    assert_eq!(build.artifact_id, 1);
    assert_eq!(build.run_id, 1);
  }

  #[test]
  fn rejects_stale_downloads_and_reports_missing_updater_metadata() {
    let directory = DownloadedPrBundle::new().unwrap();
    let revision = "a".repeat(40);
    let bundle_id = format!("0.1.2-dev.{}", &revision[..12]);
    let targets = super::super::SUPPORTED_TARGETS
      .into_iter()
      .map(|target| {
        (
          target,
          json!({
            "archive": format!("ctl-agent-bundle-{bundle_id}-{target}.tar.gz"),
            "sha256": "0".repeat(64)
          }),
        )
      })
      .collect::<BTreeMap<_, _>>();
    let bytes = serde_json::to_vec(&json!({
      "schema_version": 1,
      "app_version": "0.1.2",
      "bundle_id": bundle_id,
      "git_revision": revision,
      "targets": targets
    }))
    .unwrap();
    std::fs::write(directory.directory().join(BUNDLE_SET_FILE), bytes).unwrap();
    assert!(matches!(
      verify_download(directory.directory(), &"b".repeat(40)),
      Err(Error::Stale(_))
    ));
    assert!(matches!(
      verify_download(directory.directory(), &revision),
      Err(Error::NotAvailable(message)) if message.contains("missing the ctlup updater")
    ));
  }
}
