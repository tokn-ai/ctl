//! Platform-neutral metadata and artifact transport for the official repository.

use super::{Error, download_error};
use reqwest::{Client, Url, header};
use serde::de::DeserializeOwned;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

const REPOSITORY: &str = "tokn-ai/ctl";
const MAX_GITHUB_JSON_BYTES: usize = 4 * 1024 * 1024;

pub(super) struct Github(Client);

impl Github {
  pub(super) fn new() -> Result<Self, Error> {
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

  pub(super) async fn get<T: DeserializeOwned>(
    &self,
    path: &str,
    query: &[(&str, &str)],
  ) -> Result<T, Error> {
    let mut url = Url::parse(&format!("https://api.github.com/repos/{REPOSITORY}/{path}"))
      .map_err(|error| Error::Invalid(error.to_string()))?;
    if !query.is_empty() {
      url.query_pairs_mut().extend_pairs(query);
    }
    self.read_json(&url).await
  }

  pub(super) async fn read_json<T: DeserializeOwned>(&self, url: &Url) -> Result<T, Error> {
    let bytes = super::download(&self.0, url.as_str(), MAX_GITHUB_JSON_BYTES).await?;
    serde_json::from_slice(&bytes)
      .map_err(|_| Error::Invalid("GitHub returned invalid build metadata".into()))
  }
}

pub(super) async fn download_artifact(
  run_id: u64,
  name: &str,
  destination: &Path,
) -> Result<(), Error> {
  let status = tokio::process::Command::new("gh")
    .args([
      "run",
      "download",
      &run_id.to_string(),
      "--repo",
      "github.com/tokn-ai/ctl",
    ])
    .args(["--name", name, "--dir"])
    .arg(destination)
    .env("GH_PROMPT_DISABLED", "1")
    .env("TMPDIR", destination)
    .env("TMP", destination)
    .env("TEMP", destination)
    .kill_on_drop(true)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .status()
    .await
    .map_err(|error| {
      if error.kind() == io::ErrorKind::NotFound {
        Error::Download(
          "install the GitHub CLI and run `gh auth login` to download workflow artifacts".into(),
        )
      } else {
        Error::Io(error)
      }
    })?;
  if !status.success() {
    return Err(Error::Download(format!(
      "GitHub CLI could not download {name} from run {run_id}; check `gh auth status`"
    )));
  }
  Ok(())
}
