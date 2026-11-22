//! Build a preview and only the agent lanes needed by this development setup.

use ctl_bundle::ctream::{AgentTarget, BuildMetadata, viewer_executables};
use serde::Deserialize;
use std::{
  collections::BTreeMap,
  fs, io,
  path::{Path, PathBuf},
  process::Stdio,
  time::SystemTime,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
  io::{AsyncBufReadExt as _, BufReader},
  process::Command,
};

const LINUX_X64: &str = AgentTarget::LinuxX64.triple();
const LINUX_ARM64: &str = AgentTarget::LinuxArm64.triple();
const WINDOWS_X64: &str = AgentTarget::WindowsX64.triple();
const PREVIEW: &str = "ctream-preview";
const VIEWER_PACKAGES: &[&str] = &[PREVIEW, "ctream-cli"];
const VIEWER_EXECUTABLES: &[&str] = &[PREVIEW, "ctream"];
const AGENT_PACKAGE: &[&str] = &["ctream-agent"];
const WAYLAND_PACKAGES: &[&str] = &["ctream-agent", "ctream-wayland-input"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Compiler {
  Cargo,
  Zig,
}

impl Compiler {
  fn subcommand(self) -> &'static str {
    match self {
      Self::Cargo => "build",
      Self::Zig => "zigbuild",
    }
  }
}

#[derive(Debug, PartialEq, Eq)]
struct AgentLane {
  target: &'static str,
  bundle_target: AgentTarget,
  compiler: Compiler,
  wayland: bool,
}

impl AgentLane {
  fn select(target: &str, native: &str) -> io::Result<Self> {
    let windows = native.contains("-pc-windows-");
    let (target, bundle_target, compiler, wayland) = match target {
      LINUX_X64 => (LINUX_X64, AgentTarget::LinuxX64, Compiler::Zig, false),
      LINUX_ARM64 => (LINUX_ARM64, AgentTarget::LinuxArm64, Compiler::Zig, false),
      "x86_64-unknown-linux-gnu" if native.starts_with("x86_64-unknown-linux-") => (
        "x86_64-unknown-linux-gnu",
        AgentTarget::LinuxX64,
        Compiler::Cargo,
        true,
      ),
      "aarch64-unknown-linux-gnu" if native.starts_with("aarch64-unknown-linux-") => (
        "aarch64-unknown-linux-gnu",
        AgentTarget::LinuxArm64,
        Compiler::Cargo,
        true,
      ),
      "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu" => {
        return Err(invalid(format!(
          "{target} needs a matching native Linux build with libxkbcommon. Choose a Linux musl target to cross-build the agent."
        )));
      }
      "x86_64-pc-windows-gnu" => (
        "x86_64-pc-windows-gnu",
        AgentTarget::WindowsX64,
        if windows {
          Compiler::Cargo
        } else {
          Compiler::Zig
        },
        false,
      ),
      WINDOWS_X64 if windows => (WINDOWS_X64, AgentTarget::WindowsX64, Compiler::Cargo, false),
      WINDOWS_X64 => {
        return Err(invalid(
          "x86_64-pc-windows-msvc requires a Windows build machine. Choose x86_64-pc-windows-gnu to cross-build the agent with Zig.",
        ));
      }
      _ => {
        return Err(invalid(format!(
          "unsupported ctream agent target {target:?}; use x86_64-unknown-linux-musl, aarch64-unknown-linux-musl or x86_64-pc-windows-gnu for cross builds"
        )));
      }
    };
    Ok(Self {
      target,
      bundle_target,
      compiler,
      wayland,
    })
  }
}

fn plan(native: &str, requested: &[String]) -> io::Result<BTreeMap<&'static str, AgentLane>> {
  let mut lanes = BTreeMap::new();
  for target in requested {
    let lane = AgentLane::select(target, native)?;
    if let Some(previous) = lanes.insert(lane.bundle_target.triple(), lane) {
      return Err(invalid(format!(
        "agent targets {} and {target} use the same ctream lane",
        previous.target
      )));
    }
  }

  let native_agent = match native {
    "x86_64-pc-windows-msvc" | "x86_64-pc-windows-gnu" => Some(native),
    "x86_64-unknown-linux-gnu" | LINUX_X64 => Some("x86_64-unknown-linux-gnu"),
    "aarch64-unknown-linux-gnu" | LINUX_ARM64 => Some("aarch64-unknown-linux-gnu"),
    _ => None,
  };
  if let Some(native_agent) = native_agent {
    let lane = AgentLane::select(native_agent, native)?;
    lanes.entry(lane.bundle_target.triple()).or_insert(lane);
  }
  Ok(lanes)
}

pub(super) struct DevelopmentBuild {
  pub(super) files: BTreeMap<String, PathBuf>,
  pub(super) metadata: BuildMetadata,
}

pub(super) async fn build(
  workspace: &Path,
  native_target: &str,
  profile: Option<&str>,
  release: bool,
  target_dir: Option<&Path>,
  requested: &[String],
) -> io::Result<DevelopmentBuild> {
  let lanes = plan(native_target, requested)?;
  let version = preview_version(workspace).await?;
  let revision = checkout_revision(workspace).await;
  let settings = Settings {
    workspace,
    profile,
    release,
    target_dir,
  };
  let mut payloads = BTreeMap::new();
  let mut viewer = settings
    .artifacts(
      Compiler::Cargo,
      native_target,
      VIEWER_PACKAGES,
      VIEWER_EXECUTABLES,
    )
    .await?;
  for (cargo_name, name) in VIEWER_EXECUTABLES
    .iter()
    .zip(viewer_executables(native_target))
  {
    let path = viewer
      .remove(*cargo_name)
      .ok_or_else(|| io::Error::other(format!("Cargo did not produce {cargo_name}")))?;
    payloads.insert(name.into(), path);
  }

  for lane in lanes.into_values() {
    let mut executables = vec!["ctream-agent"];
    let packages = if lane.wayland {
      executables.extend_from_slice(lane.bundle_target.companions());
      WAYLAND_PACKAGES
    } else {
      AGENT_PACKAGE
    };
    let agents = settings
      .artifacts(lane.compiler, lane.target, packages, &executables)
      .await?;
    for (name, path) in agents {
      let filename = if name == "ctream-agent" {
        lane.bundle_target.agent()
      } else {
        &name
      };
      payloads.insert(lane.bundle_target.relative_path(filename), path);
    }
  }
  Ok(DevelopmentBuild {
    metadata: BuildMetadata {
      version: Some(version),
      revision,
      built_at: newest_artifact_time(&payloads),
      channel: "local".into(),
    },
    files: payloads,
  })
}

#[derive(Deserialize)]
struct WorkspaceMetadata {
  packages: Vec<PackageMetadata>,
}

#[derive(Deserialize)]
struct PackageMetadata {
  name: String,
  version: String,
}

async fn preview_version(workspace: &Path) -> io::Result<String> {
  let result = Command::new("cargo")
    .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
    .current_dir(workspace)
    .stdin(Stdio::null())
    .stderr(Stdio::inherit())
    .kill_on_drop(true)
    .output()
    .await?;
  if !result.status.success() {
    return Err(io::Error::other(format!(
      "cargo metadata failed for the ctream checkout ({})",
      result.status
    )));
  }
  let metadata: WorkspaceMetadata =
    serde_json::from_slice(&result.stdout).map_err(io::Error::other)?;
  metadata
    .packages
    .into_iter()
    .find(|package| package.name == PREVIEW)
    .map(|package| package.version)
    .ok_or_else(|| invalid("cargo metadata did not include ctream-preview"))
}

async fn checkout_revision(workspace: &Path) -> Option<String> {
  if !workspace.join(".git").exists() {
    return None;
  }
  let result = Command::new("git")
    .args(["rev-parse", "--verify", "HEAD"])
    .current_dir(workspace)
    .stdin(Stdio::null())
    .kill_on_drop(true)
    .output()
    .await
    .ok()?;
  let revision = std::str::from_utf8(&result.stdout).ok()?.trim();
  (result.status.success()
    && matches!(revision.len(), 40 | 64)
    && revision.bytes().all(|byte| byte.is_ascii_hexdigit()))
  .then(|| revision.to_owned())
}

fn newest_artifact_time(payloads: &BTreeMap<String, PathBuf>) -> Option<String> {
  // Cargo can reuse the same artifacts across repeated installs.
  let newest = payloads
    .values()
    .filter_map(|path| fs::metadata(path).ok()?.modified().ok())
    .max()?;
  let timestamp = newest
    .duration_since(SystemTime::UNIX_EPOCH)
    .ok()?
    .as_secs();
  OffsetDateTime::from_unix_timestamp(timestamp.try_into().ok()?)
    .ok()?
    .format(&Rfc3339)
    .ok()
}

struct Settings<'a> {
  workspace: &'a Path,
  profile: Option<&'a str>,
  release: bool,
  target_dir: Option<&'a Path>,
}

impl Settings<'_> {
  async fn artifacts(
    &self,
    compiler: Compiler,
    target: &str,
    packages: &[&str],
    expected: &[&str],
  ) -> io::Result<BTreeMap<String, PathBuf>> {
    let mut command = Command::new("cargo");
    command
      .current_dir(self.workspace)
      .args([
        compiler.subcommand(),
        "--locked",
        "--bins",
        "--message-format=json-render-diagnostics",
        "--target",
        target,
      ])
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::inherit())
      .kill_on_drop(true);
    if let Some(profile) = self.profile {
      command.args(["--profile", profile]);
    } else if self.release {
      command.arg("--release");
    }
    if let Some(directory) = self.target_dir {
      command.arg("--target-dir").arg(directory);
    }
    for package in packages {
      command.args(["--package", package]);
    }

    let subcommand = compiler.subcommand();
    let mut child = command.spawn().map_err(|error| {
      io::Error::new(
        error.kind(),
        format!("could not start cargo {subcommand} for {target}: {error}"),
      )
    })?;
    let stdout = child
      .stdout
      .take()
      .ok_or_else(|| io::Error::other("Cargo build output was not captured"))?;
    let mut lines = BufReader::new(stdout).lines();
    let mut found = BTreeMap::new();
    while let Some(line) = lines.next_line().await? {
      match serde_json::from_str::<CargoMessage>(&line) {
        Ok(message) => {
          if let Some((name, path)) = message.executable(expected) {
            let path = self.workspace.join(path);
            if found
              .insert(name.clone(), path.clone())
              .is_some_and(|previous| previous != path)
            {
              return Err(invalid(format!(
                "Cargo reported multiple development executables for {name}"
              )));
            }
          }
        }
        Err(_) => eprintln!("{line}"),
      }
    }
    let status = child.wait().await?;
    if !status.success() {
      let hint = if compiler == Compiler::Zig {
        format!(" Check that Zig, cargo-zigbuild, and the Rust target {target} are installed.")
      } else {
        String::new()
      };
      return Err(io::Error::other(format!(
        "cargo {subcommand} failed for {target} ({status}).{hint}"
      )));
    }
    for name in expected {
      if !found.get(*name).is_some_and(|path| path.is_file()) {
        return Err(io::Error::new(
          io::ErrorKind::NotFound,
          format!("Cargo did not produce the {name} executable for {target}"),
        ));
      }
    }
    Ok(found)
  }
}

#[derive(Deserialize)]
struct CargoMessage {
  reason: String,
  target: Option<CargoTarget>,
  executable: Option<PathBuf>,
}

#[derive(Deserialize)]
struct CargoTarget {
  name: String,
  kind: Vec<String>,
}

impl CargoMessage {
  fn executable(self, expected: &[&str]) -> Option<(String, PathBuf)> {
    let target = self.target?;
    if self.reason != "compiler-artifact"
      || !target.kind.iter().any(|kind| kind == "bin")
      || !expected.contains(&target.name.as_str())
    {
      return None;
    }
    Some((target.name, self.executable?))
  }
}

fn invalid(message: impl Into<String>) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn plans_only_requested_foreign_lanes_and_defaults_to_the_native_agent() {
    let mac = "aarch64-apple-darwin";
    let linux = "aarch64-unknown-linux-gnu";
    assert!(plan(mac, &[]).unwrap().is_empty());
    let lanes = plan(linux, &["x86_64-pc-windows-gnu".into()]).unwrap();
    assert!(lanes[LINUX_ARM64].wayland);
    assert_eq!(lanes[WINDOWS_X64].compiler, Compiler::Zig);
    assert_eq!(lanes[WINDOWS_X64].target, "x86_64-pc-windows-gnu");
    assert_eq!(lanes.len(), 2);

    let musl = plan(linux, &[LINUX_ARM64.into()]).unwrap();
    assert!(!musl[LINUX_ARM64].wayland);
    assert_eq!(musl[LINUX_ARM64].compiler, Compiler::Zig);
    assert!(plan(mac, &[WINDOWS_X64.into()]).is_err());
    assert!(plan(mac, &[linux.into()]).is_err());
    assert!(plan(linux, &[linux.into(), LINUX_ARM64.into()]).is_err());
  }
}
