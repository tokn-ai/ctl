//! Native development builds use the same verified installation as releases.

use ctl_core::component::protocols_match;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncReadExt as _;

const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone)]
pub struct DevelopmentOptions {
  pub workspace: PathBuf,
  pub cargo_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentResult {
  pub schema_version: u16,
  pub bundle_id: String,
  pub target_triple: String,
  pub executables: BTreeMap<String, PathBuf>,
  pub services_preserved: bool,
}

/// A prepared launch pins its immutable helpers until the caller hands off to
/// processes that acquire their own leases.
#[derive(Debug)]
pub struct PreparedDevelopment {
  pub result: DevelopmentResult,
  _lease: ctl_core::installations::Lease,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildOutput {
  schema_version: u16,
  target_triple: String,
  binary_directory: PathBuf,
  ctld_package: Option<PathBuf>,
  manager_executable: PathBuf,
}

/// Build, verify and activate a complete native checkout without restarting any
/// owner. The platform recipe only produces snapshots; installation belongs to
/// this shared engine, whether invoked by ctlup or the development launcher.
///
/// # Errors
/// Reports build/signing failures, mixed source identities, unsafe outputs and
/// activation failures. Incomplete builds never change the active installation.
pub async fn install(home: &Path, options: DevelopmentOptions) -> io::Result<PreparedDevelopment> {
  let workspace = options.workspace.canonicalize()?;
  let recipe = workspace.join("scripts/dev/build-local-development.mts");
  if !workspace.join("Cargo.toml").is_file() || !recipe.is_file() {
    return Err(invalid(
      "choose a ctl checkout containing the native development builder",
    ));
  }
  if options.cargo_args.len() > 256
    || options.cargo_args.iter().map(String::len).sum::<usize>() > MAX_DESCRIPTOR_BYTES
  {
    return Err(invalid("development Cargo arguments exceed their limit"));
  }
  let output = OutputDirectory::new()?;
  let caller = std::env::current_dir()?.canonicalize()?;
  let cargo_cwd = if caller.starts_with(&workspace) {
    caller
  } else {
    workspace.clone()
  };
  let mut command = tokio::process::Command::new(
    std::env::var_os("CTL_NODE_PROGRAM").unwrap_or_else(|| "node".into()),
  );
  command
    .args([
      "--experimental-strip-types",
      "--disable-warning=ExperimentalWarning",
    ])
    .arg(recipe)
    .arg("--workspace")
    .arg(&workspace)
    .arg("--cargo-cwd")
    .arg(&cargo_cwd)
    .arg("--native-target")
    .arg(ctl_core::paths::native_target())
    .arg("--output")
    .arg(&output.0)
    .arg("--cargo-args-json")
    .arg(serde_json::to_string(&options.cargo_args).map_err(io::Error::other)?)
    .current_dir(&cargo_cwd)
    .env("CTLUP_DEVELOPMENT_LIFELINE", "1")
    .env_remove("CTL_BUNDLED_CTLD_DIR")
    .env_remove("CTL_BUNDLED_CTLD_MODE")
    .env_remove("CTMUX_DEV_APP_SUPERVISOR")
    .env_remove("CTMUX_DEV_CTLD_EXECUTABLE")
    .env_remove("CTMUX_DEV_APP_FAILURE_MARKER");
  run_builder(command).await?;
  let descriptor = read_output(&output.0)?;
  let bundle = crate::components::import_local_for_target(
    home,
    &descriptor.binary_directory,
    descriptor.ctld_package.as_deref(),
    &descriptor.target_triple,
  )
  .await?;
  if bundle.manifest.target_triple != descriptor.target_triple {
    return Err(invalid(
      "the development build does not match this updater's native target",
    ));
  }
  let updater = ctl_core::executable::inspect(&descriptor.manager_executable).await?;
  let runtime = bundle
    .manifest
    .components
    .get("ctl")
    .filter(|_| bundle.manifest.schema_version == 2)
    .ok_or_else(|| invalid("development builds must include ctl and ctmux"))?;
  if updater.build != runtime.build || !protocols_match(&updater.protocols, &runtime.protocols) {
    return Err(invalid(
      "the development updater and runtime must come from the same build",
    ));
  }
  let (installed, lease) = crate::component_update::install_local_with_manager(
    home,
    &bundle,
    Some(&descriptor.manager_executable),
  )
  .await?;
  let mut executables = BTreeMap::new();
  for name in ctl_core::bundle_layout::ALL {
    let executable = if name == "ctld" && cfg!(target_os = "macos") {
      installed.directory.join("ctld.app/Contents/MacOS/ctld")
    } else {
      installed.directory.join(name)
    };
    executables.insert(name.into(), executable);
  }
  executables.insert("ctlup".into(), home.join(".tokn/ctl/bin/ctlup"));
  Ok(PreparedDevelopment {
    result: DevelopmentResult {
      schema_version: 1,
      bundle_id: installed.manifest.bundle_id,
      target_triple: installed.manifest.target_triple,
      executables,
      services_preserved: true,
    },
    _lease: lease,
  })
}

fn read_output(output: &Path) -> io::Result<BuildOutput> {
  let bytes = crate::components::read_input(&output.join("result.json"), MAX_DESCRIPTOR_BYTES)?;
  let descriptor: BuildOutput = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  if descriptor.schema_version != 1
    || descriptor.binary_directory != output.join("bin")
    || descriptor.manager_executable != output.join("bin/ctlup")
    || descriptor
      .ctld_package
      .as_ref()
      .is_some_and(|path| *path != output.join("helper"))
    || !ctl_core::bundles::local_target(&descriptor.target_triple)
    || (cfg!(target_os = "macos") && descriptor.ctld_package.is_none())
  {
    return Err(invalid("invalid native development build result"));
  }
  Ok(descriptor)
}

struct OutputDirectory(PathBuf);

impl OutputDirectory {
  fn new() -> io::Result<Self> {
    let directory =
      std::env::temp_dir().join(format!("ctlup-development-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
    Ok(Self(directory.canonicalize()?))
  }
}

impl Drop for OutputDirectory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

struct BuildGroup(Option<rustix::process::Pid>);

impl Drop for BuildGroup {
  fn drop(&mut self) {
    if let Some(group) = self.0 {
      let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
  }
}

async fn run_builder(mut command: tokio::process::Command) -> io::Result<()> {
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .process_group(0)
    .kill_on_drop(true);
  let mut child = command.spawn().map_err(|error| {
    io::Error::other(format!(
      "could not start the native development builder (Node 24 and Cargo are required): {error}",
    ))
  })?;
  let mut group = BuildGroup(
    child
      .id()
      .and_then(|pid| rustix::process::Pid::from_raw(i32::try_from(pid).ok()?)),
  );
  let _lifeline = child
    .stdin
    .take()
    .ok_or_else(|| io::Error::other("missing development build lifeline"))?;
  let stdout = child
    .stdout
    .take()
    .ok_or_else(|| io::Error::other("missing build output"))?;
  let stderr = child
    .stderr
    .take()
    .ok_or_else(|| io::Error::other("missing build diagnostics"))?;
  let (status, (), diagnostics) =
    tokio::try_join!(child.wait(), drain(stdout), diagnostics(stderr))?;
  group.0 = None;
  if !status.success() {
    return Err(io::Error::other(format!(
      "native development build exited {status}; the active installation was kept.\n{}",
      diagnostics.trim(),
    )));
  }
  Ok(())
}

async fn drain(mut reader: impl tokio::io::AsyncRead + Unpin) -> io::Result<()> {
  tokio::io::copy(&mut reader, &mut tokio::io::sink()).await?;
  Ok(())
}

async fn diagnostics(mut reader: impl tokio::io::AsyncRead + Unpin) -> io::Result<String> {
  use tokio::io::AsyncWriteExt as _;
  let mut tail = Vec::new();
  let mut chunk = [0_u8; 4096];
  let mut output = tokio::io::stderr();
  loop {
    let count = reader.read(&mut chunk).await?;
    if count == 0 {
      break;
    }
    let _ = output.write_all(&chunk[..count]).await;
    tail.extend_from_slice(&chunk[..count]);
    if tail.len() > MAX_DIAGNOSTIC_BYTES {
      tail.drain(..tail.len() - MAX_DIAGNOSTIC_BYTES);
    }
  }
  Ok(String::from_utf8_lossy(&tail).into_owned())
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
