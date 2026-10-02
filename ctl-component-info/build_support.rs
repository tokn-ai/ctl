use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

// Never inspect runtime configuration, credentials, environment files, user
// data, or build output. Embedded scripts and skills are component sources.
const COMPONENT_ROOTS: &[&str] = &[
  "ctl-component-info",
  "ctl-paths",
  "ctl",
  "ctmux",
  "task",
  "ctmux-process-info",
];

pub struct BuildIdentity {
  pub fingerprint: String,
  pub revision: Option<String>,
  pub dirty: bool,
}

fn git(root: &Path, arguments: &[&str]) -> Option<String> {
  let output = Command::new("git")
    .current_dir(root)
    .args(arguments)
    .output()
    .ok()?;
  output
    .status
    .success()
    .then(|| String::from_utf8_lossy(&output.stdout).trim().into())
}

fn workspace_root(manifest_dir: &Path) -> Option<&Path> {
  // Cargo's archive provenance takes precedence even when unpacked in a checkout.
  if manifest_dir.join(".cargo_vcs_info.json").exists() {
    return None;
  }
  let root = manifest_dir.parent()?;
  (root.join("Cargo.toml").is_file()
    && root.join("Cargo.lock").is_file()
    && COMPONENT_ROOTS
      .iter()
      .all(|source| root.join(source).is_dir()))
  .then_some(root)
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
  println!("cargo:rerun-if-changed={}", directory.display());
  for entry in std::fs::read_dir(directory).expect("read component source directory") {
    let entry = entry.expect("read component source entry");
    let kind = entry.file_type().expect("read source file type");
    let path = entry.path();
    if kind.is_dir() {
      if !matches!(
        entry.file_name().to_str(),
        Some("target" | ".git" | "node_modules")
      ) {
        collect(&path, files);
      }
    } else if kind.is_file()
      && (path
        .extension()
        .is_some_and(|extension| extension == "rs" || extension == "sh")
        || path
          .file_name()
          .is_some_and(|name| name == "Cargo.toml" || name == "Cargo.toml.orig")
        || (path.extension().is_some_and(|extension| extension == "md")
          && path
            .components()
            .any(|component| component.as_os_str() == "skills")))
    {
      files.push(path);
    }
  }
}

fn fingerprint(root: &Path, files: &mut [PathBuf], packaged: bool) -> String {
  files.sort();
  let mut hash = Sha256::new();
  if packaged {
    // A registry package hashes its own sources, not the unavailable workspace.
    hash.update(b"ctl-component-info package\0");
  }
  for path in files {
    println!("cargo:rerun-if-changed={}", path.display());
    let relative = path
      .strip_prefix(root)
      .unwrap()
      .to_string_lossy()
      .replace('\\', "/");
    let contents = std::fs::read_to_string(path)
      .expect("read component source")
      .replace("\r\n", "\n");
    hash.update(relative.as_bytes());
    hash.update([0]);
    hash.update(u64::try_from(contents.len()).unwrap().to_le_bytes());
    hash.update(contents.as_bytes());
  }
  format!("{:x}", hash.finalize())
}

fn package_provenance(manifest_dir: &Path) -> (Option<String>, bool) {
  let path = manifest_dir.join(".cargo_vcs_info.json");
  println!("cargo:rerun-if-changed={}", path.display());
  let metadata = std::fs::read(path)
    .ok()
    .and_then(|contents| serde_json::from_slice::<serde_json::Value>(&contents).ok());
  let revision = metadata
    .as_ref()
    .and_then(|metadata| metadata["git"]["sha1"].as_str())
    .filter(|revision| {
      matches!(revision.len(), 40 | 64) && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
    .map(str::to_owned);
  let dirty = revision.is_none()
    || metadata.as_ref().is_none_or(|metadata| {
      // Cargo omits `dirty` for a clean package.
      let dirty = &metadata["git"]["dirty"];
      !dirty.is_null() && dirty.as_bool() != Some(false)
    });
  (revision, dirty)
}

fn workspace_provenance(root: &Path) -> (Option<String>, bool) {
  // An exported workspace must not inherit provenance from an enclosing repo.
  let own_repository = git(root, &["rev-parse", "--show-toplevel"])
    .and_then(|path| std::fs::canonicalize(path).ok())
    == std::fs::canonicalize(root).ok();
  if !own_repository {
    return (None, true);
  }
  for reference in [
    Some("HEAD".into()),
    git(root, &["symbolic-ref", "-q", "HEAD"]),
  ]
  .into_iter()
  .flatten()
  {
    if let Some(path) = git(root, &["rev-parse", "--git-path", &reference]) {
      println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
  }
  let mut arguments = vec!["status", "--porcelain", "--untracked-files=normal", "--"];
  arguments.extend_from_slice(COMPONENT_ROOTS);
  arguments.extend(["Cargo.toml", "Cargo.lock"]);
  let dirty = git(root, &arguments).is_none_or(|status| !status.is_empty());
  (git(root, &["rev-parse", "HEAD"]), dirty)
}

pub fn read_identity(manifest_dir: &Path) -> BuildIdentity {
  let workspace = workspace_root(manifest_dir);
  let root = workspace.unwrap_or(manifest_dir);
  let mut files = Vec::new();
  if workspace.is_some() {
    files.push(root.join("Cargo.toml"));
    for source in COMPONENT_ROOTS {
      collect(&root.join(source), &mut files);
    }
  } else {
    collect(root, &mut files);
  }
  if root.join("Cargo.lock").is_file() {
    files.push(root.join("Cargo.lock"));
  }
  let fingerprint = fingerprint(root, &mut files, workspace.is_none());
  let (revision, dirty) = if workspace.is_some() {
    workspace_provenance(root)
  } else {
    package_provenance(root)
  };
  BuildIdentity {
    fingerprint,
    revision,
    dirty,
  }
}
