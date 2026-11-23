use std::{path::PathBuf, process::Command};

fn git(args: &[&str]) -> Option<String> {
  let output = Command::new("git").args(args).output().ok()?;
  output
    .status
    .success()
    .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
  println!("cargo:rerun-if-env-changed=CTL_DESKTOP_BUILD_CHANNEL");
  // A locally compiled app can follow the nightly update feed. Record how the
  // executable was built independently of that default and of ctlup selection.
  let channel = std::env::var("CTL_DESKTOP_BUILD_CHANNEL").unwrap_or_else(|_| "local".into());
  assert!(
    matches!(
      channel.as_str(),
      "local" | "dev" | "nightly" | "stable" | "prerelease"
    ),
    "invalid CTL_DESKTOP_BUILD_CHANNEL"
  );
  println!("cargo:rustc-env=CTMUX_DESKTOP_BUILD_CHANNEL={channel}");
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
  println!("cargo:rustc-env=CTMUX_SOURCE_REVISION={revision}");
  let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../..");
  for path in ["ctl", "ctmux", "task", "Cargo.toml", "Cargo.lock"] {
    println!("cargo:rerun-if-changed={}", root.join(path).display());
  }
  let dirty = git(&[
    "status",
    "--porcelain",
    "--untracked-files=normal",
    "--",
    "../../../ctl",
    "../../../ctmux",
    "../../../task",
    "../../../Cargo.toml",
    "../../../Cargo.lock",
  ])
  .is_none_or(|status| !status.is_empty());
  println!("cargo:rustc-env=CTMUX_COMPONENTS_DIRTY={dirty}");
  let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
  let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
  let development = ctl_core::development::context(
    &manifest,
    std::path::Path::new("apps/desktop/src-tauri"),
    &output,
    &std::env::var("TARGET").unwrap(),
  )
  .expect("could not determine local development helper provenance");
  ctl_core::development::write(&output, development.as_ref())
    .expect("could not write local development helper provenance");
  tauri_build::build();
}
