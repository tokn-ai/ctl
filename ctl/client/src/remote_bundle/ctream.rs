//! Fetch the independent ctream tool bundle from the official workflow.

use super::github::{Github, download_artifact};
use ctl_bundle::ctream::{archive_name, has_workflow_bundle};
use serde::Deserialize;
use std::{
  io,
  path::{Path, PathBuf},
};

const WORKFLOW_FILE: &str = "ctream-preview.yml";
const WORKFLOW_PATH: &str = ".github/workflows/ctream-preview.yml";
const REPOSITORY: &str = "tokn-ai/ctl";

#[derive(Deserialize, PartialEq, Eq)]
struct WorkflowRun {
  id: u64,
  run_attempt: u64,
  path: String,
  status: String,
  conclusion: Option<String>,
  event: String,
  head_branch: String,
  head_sha: String,
  repository: Repository,
  head_repository: Option<Repository>,
}

#[derive(Deserialize, PartialEq, Eq)]
struct Repository {
  id: u64,
  full_name: String,
}

#[derive(Deserialize)]
struct Runs {
  workflow_runs: Vec<WorkflowRun>,
}

impl WorkflowRun {
  fn eligible(&self, requested: Option<u64>) -> bool {
    self.id != 0
      && self.run_attempt != 0
      && self.path.split('@').next() == Some(WORKFLOW_PATH)
      && self.status == "completed"
      && self.conclusion.as_deref() == Some("success")
      && self.repository.id != 0
      && self.repository.full_name == REPOSITORY
      && self.head_repository.as_ref() == Some(&self.repository)
      && match requested {
        Some(id) => self.id == id,
        None => {
          self.head_branch == "main" && matches!(self.event.as_str(), "push" | "workflow_dispatch")
        }
      }
  }
}

/// Download the requested official workflow run or the latest successful main build.
///
/// The caller provides a private staging directory. `gh` downloads the outer
/// workflow artifact there and the caller validates the returned inner archive.
///
/// # Errors
/// Returns an error if metadata, authentication, or artifact retrieval fails.
pub async fn download(requested: Option<u64>, target: &str, staging: &Path) -> io::Result<PathBuf> {
  if !has_workflow_bundle(target) {
    return Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "unsupported ctream viewer target",
    ));
  }
  let github = Github::new().map_err(io::Error::other)?;
  let run: WorkflowRun = if let Some(id) = requested {
    github
      .get(&format!("actions/runs/{id}"), &[])
      .await
      .map_err(io::Error::other)?
  } else {
    let runs: Runs = github
      .get(
        &format!("actions/workflows/{WORKFLOW_FILE}/runs"),
        &[
          ("branch", "main"),
          ("status", "success"),
          ("per_page", "20"),
        ],
      )
      .await
      .map_err(io::Error::other)?;
    runs
      .workflow_runs
      .into_iter()
      .find(|run| run.eligible(None))
      .ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::NotFound,
          "No successful ctream bundle is available on main. Use --from with a local ctream ZIP.",
        )
      })?
  };
  if !run.eligible(requested) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "The run is not a successful ctream bundle build from the official repository",
    ));
  }
  let name = archive_name(target);
  download_artifact(run.id, &name, staging)
    .await
    .map_err(io::Error::other)?;
  let observed: WorkflowRun = github
    .get(&format!("actions/runs/{}", run.id), &[])
    .await
    .map_err(io::Error::other)?;
  if observed != run {
    return Err(io::Error::other(
      "The ctream workflow changed during download. Retry the install.",
    ));
  }
  Ok(staging.join(format!("{name}.zip")))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_ctream_download_only_accepts_successful_official_main_runs() {
    let mut run: WorkflowRun = serde_json::from_value(serde_json::json!({
      "id": 42, "run_attempt": 1, "path": WORKFLOW_PATH, "status": "completed",
      "conclusion": "success", "event": "push", "head_branch": "main", "head_sha": "fixture",
      "repository": {"id": 1, "full_name": REPOSITORY},
      "head_repository": {"id": 1, "full_name": REPOSITORY}
    }))
    .unwrap();
    assert!(run.eligible(None));
    run.head_branch = "feature".into();
    assert!(!run.eligible(None));
    assert!(run.eligible(Some(42)));
    assert!(!run.eligible(Some(43)));
    run.path = ".github/workflows/bundles.yml".into();
    assert!(!run.eligible(Some(42)));
    run.path = WORKFLOW_PATH.into();
    run.head_repository.as_mut().unwrap().id = 2;
    assert!(!run.eligible(Some(42)));
  }
}
