use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

// Only code and dependency definitions contribute. Never inspect runtime
// configuration, credentials, environment files, user data, or build output.
const COMPONENT_ROOTS: &[&str] = &[
  "ctl-component-info",
  "ctl",
  "ctmux",
  "task",
  "ctmux-process-info",
];

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

fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
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
      && (path.extension().is_some_and(|extension| extension == "rs")
        || path.file_name().is_some_and(|name| name == "Cargo.toml"))
    {
      files.push(path);
    }
  }
}

fn main() {
  let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
    .parent()
    .unwrap()
    .to_path_buf();
  let mut files = vec![root.join("Cargo.toml"), root.join("Cargo.lock")];
  for source in COMPONENT_ROOTS {
    let directory = root.join(source);
    println!("cargo:rerun-if-changed={}", directory.display());
    collect(&directory, &mut files);
  }
  files.sort();
  let mut hash = Sha256::new();
  for path in files {
    println!("cargo:rerun-if-changed={}", path.display());
    let relative = path
      .strip_prefix(&root)
      .unwrap()
      .to_string_lossy()
      .replace('\\', "/");
    let contents = std::fs::read_to_string(&path)
      .expect("read component source")
      .replace("\r\n", "\n");
    hash.update(relative.as_bytes());
    hash.update([0]);
    hash.update(u64::try_from(contents.len()).unwrap().to_le_bytes());
    hash.update(contents.as_bytes());
  }
  println!(
    "cargo:rustc-env=COMPONENT_SOURCE_FINGERPRINT={:x}",
    hash.finalize()
  );
  for reference in [
    Some("HEAD".into()),
    git(&root, &["symbolic-ref", "-q", "HEAD"]),
  ]
  .into_iter()
  .flatten()
  {
    if let Some(path) = git(&root, &["rev-parse", "--git-path", &reference]) {
      println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
  }
  println!(
    "cargo:rustc-env=COMPONENT_SOURCE_REVISION={}",
    git(&root, &["rev-parse", "HEAD"]).unwrap_or_default()
  );
  let dirty = git(
    &root,
    &[
      "status",
      "--porcelain",
      "--untracked-files=normal",
      "--",
      "ctl-component-info",
      "ctl",
      "ctmux",
      "task",
      "ctmux-process-info",
      "Cargo.toml",
      "Cargo.lock",
    ],
  )
  .is_none_or(|status| !status.is_empty());
  println!("cargo:rustc-env=COMPONENT_SOURCE_DIRTY={dirty}");
}
