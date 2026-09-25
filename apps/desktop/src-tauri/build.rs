use std::{path::PathBuf, process::Command};

fn git(args: &[&str]) -> Option<String> {
  let output = Command::new("git").args(args).output().ok()?;
  output
    .status
    .success()
    .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
  // HEAD can be a worktree-local file while its branch ref lives in the common
  // git directory. Track both so switching/committing refreshes bundle identity.
  for name in [
    Some("HEAD".to_owned()),
    git(&["symbolic-ref", "-q", "HEAD"]),
  ]
  .into_iter()
  .flatten()
  {
    if let Some(path) = git(&["rev-parse", "--git-path", &name]) {
      println!("cargo:rerun-if-changed={path}");
    }
  }
  let revision = git(&["rev-parse", "HEAD"]).unwrap_or_default();
  println!("cargo:rustc-env=RMUX_SOURCE_REVISION={revision}");
  let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../..");
  for path in ["ctl", "rmux", "task", "Cargo.toml", "Cargo.lock"] {
    println!("cargo:rerun-if-changed={}", root.join(path).display());
  }
  let dirty = git(&[
    "status",
    "--porcelain",
    "--untracked-files=normal",
    "--",
    "../../../ctl",
    "../../../rmux",
    "../../../task",
    "../../../Cargo.toml",
    "../../../Cargo.lock",
  ])
  .is_none_or(|status| !status.is_empty());
  println!("cargo:rustc-env=RMUX_COMPONENTS_DIRTY={dirty}");
  tauri_build::build();
}
