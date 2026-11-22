//! Assemble the official ctream viewer ZIP from CI build outputs.

use ctl_bundle::ctream::{
  AgentTarget, BuildMetadata, MAX_AGENT_BYTES, MAX_ARCHIVE_BYTES, MAX_PAYLOAD_BYTES, archive_name,
  has_workflow_bundle, viewer_executables,
};
use serde::Deserialize;
use std::{
  env,
  fs::{self, File},
  io::{self, Read as _},
  path::{Path, PathBuf},
  process::{Command, ExitCode},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

fn main() -> ExitCode {
  match pack() {
    Ok(path) => {
      println!("{}", path.display());
      ExitCode::SUCCESS
    }
    Err(error) => {
      eprintln!("ctl-pack-ctream: {error}");
      ExitCode::FAILURE
    }
  }
}

fn pack() -> io::Result<PathBuf> {
  let args = env::args_os().skip(1).collect::<Vec<_>>();
  let [target, viewer_dir, agent_artifacts, output_dir] = args.as_slice() else {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "usage: ctl-pack-ctream TARGET VIEWER_BIN_DIR AGENT_ARTIFACTS_DIR OUTPUT_DIR",
    ));
  };
  let target = target
    .to_str()
    .filter(|target| has_workflow_bundle(target))
    .ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::InvalidInput,
        "unsupported ctream viewer target",
      )
    })?;
  let inputs = bundle_inputs(target, Path::new(viewer_dir), Path::new(agent_artifacts));
  let name = archive_name(target);
  let comment = build_metadata()?.archive_comment()?;
  fs::create_dir_all(output_dir)?;
  let path = Path::new(output_dir).join(format!("{name}.zip"));
  let file = File::create_new(&path)?;
  if let Err(error) = write_archive(file, &name, &inputs, comment) {
    let _ = fs::remove_file(&path);
    return Err(error);
  }
  Ok(path)
}

#[derive(Deserialize)]
struct Workspace {
  packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
  name: String,
  version: String,
}

fn preview_version() -> io::Result<String> {
  let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
  let output = Command::new(cargo)
    .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
    .output()?;
  if !output.status.success() {
    return Err(io::Error::other(
      "could not read ctream preview metadata from Cargo",
    ));
  }
  let workspace: Workspace = serde_json::from_slice(&output.stdout).map_err(io::Error::other)?;
  workspace
    .packages
    .into_iter()
    .find(|package| package.name == "ctream-preview")
    .map(|package| package.version)
    .ok_or_else(|| io::Error::other("Cargo workspace does not contain ctream-preview"))
}

fn build_metadata() -> io::Result<BuildMetadata> {
  let official = env::var_os("GITHUB_ACTIONS").is_some_and(|value| value == "true");
  let version = match preview_version() {
    Ok(version) => Some(version),
    Err(_) if !official => None,
    Err(error) => return Err(error),
  };
  if !official {
    return Ok(BuildMetadata {
      version,
      revision: None,
      built_at: None,
      channel: "provided".into(),
    });
  }
  let event = env::var("GITHUB_EVENT_NAME").unwrap_or_default();
  let reference = env::var("GITHUB_REF").unwrap_or_default();
  let channel = workflow_channel(&event, &reference);
  Ok(BuildMetadata {
    version,
    revision: Some(env::var("GITHUB_SHA").map_err(io::Error::other)?),
    built_at: Some(
      OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(io::Error::other)?,
    ),
    channel,
  })
}

fn workflow_channel(event: &str, reference: &str) -> String {
  if reference == "refs/heads/main" && matches!(event, "push" | "workflow_dispatch") {
    return "nightly".into();
  }
  if event == "pull_request"
    && let Some(reference) = reference.strip_prefix("refs/pull/")
    && let Some((number, "merge" | "head")) = reference.split_once('/')
    && let Ok(number) = number.parse::<u64>()
    && number != 0
  {
    return format!("pr-{number}");
  }
  "provided".into()
}

fn bundle_inputs(
  target: &str,
  viewer_dir: &Path,
  agent_artifacts: &Path,
) -> Vec<(String, PathBuf)> {
  let viewer_inputs = viewer_executables(target)
    .into_iter()
    .map(|name| (name.to_owned(), viewer_dir.join(name)));
  let agent_inputs = AgentTarget::ALL.into_iter().flat_map(|agent| {
    let dir = agent_artifacts.join(format!("ctream-agent-files-{}", agent.triple()));
    std::iter::once(agent.agent())
      .chain(agent.companions().iter().copied())
      .map(move |name| (agent.relative_path(name), dir.join(name)))
  });
  viewer_inputs.chain(agent_inputs).collect()
}

fn write_archive(
  file: File,
  name: &str,
  inputs: &[(String, PathBuf)],
  comment: String,
) -> io::Result<()> {
  let mut archive = ZipWriter::new(file);
  archive.set_comment(comment)?;
  let options = SimpleFileOptions::default()
    .compression_method(CompressionMethod::Deflated)
    .unix_permissions(0o755);
  let mut remaining = MAX_PAYLOAD_BYTES;
  for (entry, path) in inputs {
    let file = File::open(path)
      .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))?;
    let metadata = file.metadata()?;
    let size = metadata.len();
    if !metadata.is_file() || !(1..=MAX_AGENT_BYTES.min(remaining)).contains(&size) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("invalid ctream bundle input {}", path.display()),
      ));
    }
    archive.start_file(format!("{name}/{entry}"), options)?;
    if io::copy(&mut file.take(size + 1), &mut archive)? != size {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
          "ctream bundle input changed while reading {}",
          path.display()
        ),
      ));
    }
    remaining -= size;
  }
  let file = archive.finish()?;
  if file.metadata()?.len() > MAX_ARCHIVE_BYTES {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "ctream ZIP exceeds the installer size limit",
    ));
  }
  file.sync_all()
}
