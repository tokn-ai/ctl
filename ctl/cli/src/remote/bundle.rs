//! Matching local, published, or existing CI remote component bundles.

use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use ctl_client::remote_bundle::{self, VerifiedBundle};
use ctl_core::component::ComponentBuildInfo;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;

use super::Error;

#[path = "bundle/artifact.rs"]
mod artifact;

const REPOSITORY: &str = "github.com/tokn-ai/ctl";
const MAX_GH_OUTPUT: u64 = 128 * 1024;
const LIST_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) async fn matching_bundle(target: &str) -> Result<VerifiedBundle, Error> {
  #[cfg(unix)]
  if let Some(home) = dirs::home_dir()
    && let Some(selected) = selected_upload(&home, target).await?
  {
    return Ok(selected);
  }
  let expected = ctl_core::component::build_info();
  let directories = bundle_directories()?;
  let explicit = std::env::var_os("CTL_REMOTE_BUNDLES_DIR").is_some();
  let bundle = if let Some(bundle) = local_bundle(target, &expected, directories, explicit).await? {
    bundle
  } else {
    let legacy = if let Some(home) = dirs::home_dir() {
      let target = target.to_owned();
      let expected = expected.clone();
      tokio::task::spawn_blocking(move || {
        remote_bundle::read_compatible_cached_bundle(
          &home.join(".tokn/ctl/agent-bundles"),
          &target,
          &expected,
        )
      })
      .await??
    } else {
      None
    };
    match legacy {
      Some(bundle) => {
        report_bundle(&bundle, "cached");
        bundle
      }
      None => download_bundle(target, &expected).await?,
    }
  };
  #[cfg(unix)]
  if let Some(home) = dirs::home_dir() {
    let source = if bundle.bundle_id == bundle.app_version {
      ctl_core::bundles::Source::Release
    } else {
      ctl_core::bundles::Source::Ci
    };
    let target = target.to_owned();
    let import_home = home.clone();
    let imported = tokio::task::spawn_blocking(move || {
      ctl_client::components::import_remote(&import_home, &bundle, &target, source)
    })
    .await??;
    let selected = ctl_client::components::initialize_upload(&home, &imported).await?;
    return Ok(
      tokio::task::spawn_blocking(move || ctl_client::components::upload_bundle(&selected))
        .await??,
    );
  }
  Ok(bundle)
}

fn report_bundle(bundle: &VerifiedBundle, origin: &str) {
  eprintln!(
    "ctl: Using {origin} remote components {} ({})",
    bundle.app_version,
    &bundle.git_revision[..12]
  );
}

#[cfg(unix)]
async fn selected_upload(
  home: &std::path::Path,
  target: &str,
) -> Result<Option<VerifiedBundle>, Error> {
  let home = home.to_owned();
  let target = target.to_owned();
  Ok(
    tokio::task::spawn_blocking(move || {
      ctl_core::bundles::Store::new(&home)
        .selected(ctl_core::bundles::Purpose::Upload, &target)?
        .map(|selected| {
          let bundle = ctl_client::components::upload_bundle(&selected)?;
          report_bundle(&bundle, "selected");
          Ok::<_, io::Error>(bundle)
        })
        .transpose()
    })
    .await??,
  )
}

async fn download_bundle(
  target: &str,
  expected: &ComponentBuildInfo,
) -> Result<VerifiedBundle, Error> {
  eprintln!("ctl: Downloading the matching official release bundle...");
  match remote_bundle::download_release_bundle(target, expected).await {
    Ok(bundle) => Ok(bundle),
    Err(release_error) if permits_artifact_fallback(&release_error) => {
      eprintln!("ctl: Looking for existing CI bundles at this source revision...");
      download_artifact_bundle(target, expected, OsStr::new("gh"))
        .await
        .map_err(|artifact_error| unavailable_after_release(&release_error, &artifact_error).into())
    }
    Err(error) => Err(error.into()),
  }
}

fn require_clean_build(expected: &ComponentBuildInfo) -> Result<(), remote_bundle::Error> {
  if expected.dirty || expected.source_revision.is_none() || !expected.is_valid() {
    return Err(remote_bundle::Error::Stale(
      "remote component recovery requires a clean client built from an identified source revision; commit component changes and rebuild ctl".into(),
    ));
  }
  Ok(())
}

fn permits_artifact_fallback(error: &remote_bundle::Error) -> bool {
  matches!(
    error,
    remote_bundle::Error::NotAvailable(_) | remote_bundle::Error::Stale(_)
  )
}

fn unavailable_after_release(
  release_error: &remote_bundle::Error,
  artifact_error: &remote_bundle::Error,
) -> remote_bundle::Error {
  remote_bundle::Error::NotAvailable(format!(
    "{release_error}. Existing CI bundles could not be used: {artifact_error}. Run `pnpm agents:sync` from apps/desktop at this clean, pushed revision, then retry; or set CTL_REMOTE_BUNDLES_DIR to its matching bundle set",
  ))
}

async fn local_bundle(
  target: &str,
  expected: &ComponentBuildInfo,
  directories: Vec<PathBuf>,
  explicit: bool,
) -> Result<Option<VerifiedBundle>, Error> {
  let target = target.to_owned();
  let expected = expected.clone();
  let bundle = tokio::task::spawn_blocking(move || {
    remote_bundle::read_reusable_bundle(&directories, &target, &expected)
  })
  .await??;
  if bundle.is_none() && explicit {
    return Err(
      remote_bundle::Error::NotAvailable(
        "CTL_REMOTE_BUNDLES_DIR contains no reusable bundle for the remote target".into(),
      )
      .into(),
    );
  }
  Ok(bundle)
}

pub(super) fn bundle_directories() -> io::Result<Vec<PathBuf>> {
  if let Some(directory) = std::env::var_os("CTL_REMOTE_BUNDLES_DIR") {
    return Ok(vec![PathBuf::from(directory)]);
  }
  let mut directories = Vec::new();
  if cfg!(debug_assertions) {
    directories.push(
      PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/desktop/src-tauri/resources/agent-bundles"),
    );
  }
  if let Some(parent) = std::env::current_exe()?.parent() {
    directories.push(parent.join("resources/agent-bundles"));
  }
  if let Some(home) = dirs::home_dir() {
    directories.push(home.join(".tokn/ctl/agent-bundles"));
  }
  Ok(directories)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WorkflowRun {
  database_id: u64,
  head_sha: String,
  status: String,
  conclusion: Option<String>,
}

fn select_runs(bytes: &[u8], revision: &str) -> Result<Vec<u64>, remote_bundle::Error> {
  let runs: Vec<WorkflowRun> = serde_json::from_slice(bytes).map_err(|_| {
    remote_bundle::Error::Invalid("GitHub returned an invalid bundle workflow run list".into())
  })?;
  if runs.len() > 20
    || runs.iter().any(|run| {
      run.database_id == 0
        || run.head_sha.len() != 40
        || !run.head_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
        || run.status.len() > 64
        || run
          .conclusion
          .as_ref()
          .is_some_and(|value| value.len() > 64)
    })
  {
    return Err(remote_bundle::Error::Invalid(
      "GitHub returned invalid bundle workflow metadata".into(),
    ));
  }
  let candidates: Vec<_> = ["success", "failure"]
    .into_iter()
    .flat_map(|conclusion| {
      runs
        .iter()
        .filter(move |run| {
          run.head_sha == revision
            && run.status == "completed"
            && run.conclusion.as_deref() == Some(conclusion)
        })
        .map(|run| run.database_id)
    })
    .collect();
  if candidates.is_empty() {
    return Err(remote_bundle::Error::NotAvailable(
      "no completed bundle workflow matches this client source revision".into(),
    ));
  }
  Ok(candidates)
}

async fn download_artifact_bundle(
  target: &str,
  expected: &ComponentBuildInfo,
  program: &OsStr,
) -> Result<VerifiedBundle, remote_bundle::Error> {
  require_clean_build(expected)?;
  let revision = expected
    .source_revision
    .as_deref()
    .ok_or_else(|| remote_bundle::Error::Stale("client source revision is unavailable".into()))?;
  let output = run_gh(
    program,
    &[
      OsStr::new("run"),
      OsStr::new("list"),
      OsStr::new("--workflow"),
      OsStr::new("bundles.yml"),
      OsStr::new("--commit"),
      OsStr::new(revision),
      OsStr::new("--status"),
      OsStr::new("completed"),
      OsStr::new("--limit"),
      OsStr::new("20"),
      OsStr::new("--repo"),
      OsStr::new(REPOSITORY),
      OsStr::new("--json"),
      OsStr::new("databaseId,headSha,status,conclusion"),
    ],
    LIST_TIMEOUT,
  )
  .await?;
  let candidates = select_runs(&output, revision)?;
  let mut unavailable =
    remote_bundle::Error::NotAvailable("no matching workflow artifact is available".into());
  for run in candidates {
    match artifact::download(program, run, target, expected).await {
      Ok(bundle) => return Ok(bundle),
      Err(error @ remote_bundle::Error::NotAvailable(_)) => unavailable = error,
      Err(error) => return Err(error),
    }
  }
  Err(unavailable)
}

async fn read_capped(reader: impl AsyncRead + Unpin) -> io::Result<Vec<u8>> {
  let mut bytes = Vec::new();
  reader
    .take(MAX_GH_OUTPUT + 1)
    .read_to_end(&mut bytes)
    .await?;
  if bytes.len() as u64 > MAX_GH_OUTPUT {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "GitHub command output exceeds its size limit",
    ));
  }
  Ok(bytes)
}

async fn run_gh(
  program: &OsStr,
  arguments: &[&OsStr],
  timeout: Duration,
) -> Result<Vec<u8>, remote_bundle::Error> {
  let (mut child, stdout, stderr) = spawn_gh(program, arguments)?;
  let operation = async {
    let (status, stdout, stderr) =
      tokio::try_join!(child.wait(), read_capped(stdout), read_capped(stderr))?;
    if !status.success() {
      return Err(io::Error::other(format!(
        "GitHub command failed ({status}): {}",
        crate::table::text(&String::from_utf8_lossy(&stderr)),
      )));
    }
    Ok(stdout)
  };
  match tokio::time::timeout(timeout, operation).await {
    Ok(Ok(output)) => Ok(output),
    Ok(Err(error)) => {
      let _ = child.start_kill();
      let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
      Err(remote_bundle::Error::Download(error.to_string()))
    }
    Err(_) => {
      let _ = child.start_kill();
      let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
      Err(remote_bundle::Error::Download(
        "GitHub command stopped responding before its deadline".into(),
      ))
    }
  }
}

fn spawn_gh(
  program: &OsStr,
  arguments: &[&OsStr],
) -> Result<
  (
    tokio::process::Child,
    tokio::process::ChildStdout,
    tokio::process::ChildStderr,
  ),
  remote_bundle::Error,
> {
  let mut child = Command::new(program)
    .args(arguments)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .map_err(|error| {
      let detail = if error.kind() == io::ErrorKind::NotFound {
        "GitHub CLI is unavailable; install gh and authenticate it to download existing CI bundles"
          .into()
      } else {
        format!("could not start GitHub CLI: {error}")
      };
      remote_bundle::Error::Download(detail)
    })?;
  let stdout = child
    .stdout
    .take()
    .ok_or_else(|| remote_bundle::Error::Download("GitHub command has no stdout".into()))?;
  let stderr = child
    .stderr
    .take()
    .ok_or_else(|| remote_bundle::Error::Download("GitHub command has no stderr".into()))?;
  Ok((child, stdout, stderr))
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
  fn new() -> io::Result<Self> {
    let directory =
      std::env::temp_dir().join(format!("ctl-remote-ci-bundle-{}", uuid::Uuid::new_v4()));
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    {
      use std::os::unix::fs::DirBuilderExt as _;
      builder.mode(0o700);
    }
    builder.create(&directory)?;
    Ok(Self(directory))
  }
}

impl Drop for TemporaryDirectory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[cfg(test)]
#[path = "bundle/tests.rs"]
mod tests;
