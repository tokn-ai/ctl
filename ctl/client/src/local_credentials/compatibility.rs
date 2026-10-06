//! Validate the selected binary's explicit operation contract before sending input.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ctl_core::component::{ComponentInfo, ProtocolInfo};
use ctl_core::protocol::ProtocolVersion;
use tokio::process::Command;
use zeroize::Zeroizing;

use super::{Error, exchange};

const MAX_METADATA_BYTES: usize = 16 * 1024;
const METADATA_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) struct CheckedHelper {
  executable: PathBuf,
  protocol: ProtocolInfo,
  required: ProtocolVersion,
}

impl CheckedHelper {
  pub fn rejected_operation(&self) -> Error {
    Error::with_message(
      "credential_helper_unsupported",
      format!(
        "Selected ctld helper {} advertises ctld_helper {} (build {}), but rejected an operation requiring {} (build {}). Update or rebuild that helper and try again.",
        executable_label(&self.executable),
        self.protocol.version,
        self.protocol.build,
        self.required,
        self.required.build,
      ),
    )
  }

  pub fn transport_failure(&self, error: Error) -> Error {
    let message = error.message.into_owned();
    Error::with_message(
      error.code,
      format!(
        "{} Selected ctld helper {} advertises ctld_helper {} (build {}); this operation requires {} (build {}).",
        message,
        executable_label(&self.executable),
        self.protocol.version,
        self.protocol.build,
        self.required,
        self.required.build,
      ),
    )
  }
}

pub(super) async fn check(
  command: Command,
  required: ProtocolVersion,
) -> Result<(Command, CheckedHelper), Error> {
  let selected = command.as_std();
  let executable = resolve(selected).map_err(|_| {
    metadata_failure(
      Path::new(selected.get_program()),
      required,
      "credential_helper_unavailable",
    )
  })?;
  // Resolve PATH and symlinks once. Both processes then execute the same
  // canonical file even if setup switches the selected alias during inspection.
  let metadata_command = with_executable(selected, &executable);
  let output = exchange(
    metadata_command,
    "--component-info",
    Zeroizing::new(Vec::new()),
    MAX_METADATA_BYTES,
    METADATA_TIMEOUT,
  )
  .await
  .map_err(|error| metadata_failure(&executable, required, error.code))?;
  if !output.success || output.input_result.is_err() {
    return Err(metadata_failure(
      &executable,
      required,
      "credential_helper_failed",
    ));
  }
  let metadata: ComponentInfo = serde_json::from_slice(&output.bytes)
    .map_err(|_| metadata_failure(&executable, required, "credential_helper_metadata_invalid"))?;
  if !metadata.is_valid() {
    return Err(metadata_failure(
      &executable,
      required,
      "credential_helper_metadata_invalid",
    ));
  }
  let protocol = metadata
    .protocols
    .into_iter()
    .find(|protocol| protocol.name == "ctld_helper");
  let Some(protocol) = protocol else {
    let label = executable_label(&executable);
    return Err(Error::with_message(
      "credential_helper_unsupported",
      format!(
        "Selected ctld helper {label} does not advertise ctld_helper; this operation requires {required} (build {}). Update or rebuild that helper and try again.",
        required.build,
      ),
    ));
  };
  if !protocol.supports(required) {
    let label = executable_label(&executable);
    return Err(Error::with_message(
      "credential_helper_unsupported",
      format!(
        "Selected ctld helper {label} advertises ctld_helper {} (build {}) and supports [{}]; this operation requires {required} (build {}). Update or rebuild that helper and try again.",
        protocol.version,
        protocol.build,
        protocol
          .supported_versions
          .iter()
          .map(ToString::to_string)
          .collect::<Vec<_>>()
          .join(", "),
        required.build,
      ),
    ));
  }
  let pinned_command = with_executable(selected, &executable);
  Ok((
    pinned_command,
    CheckedHelper {
      executable,
      protocol,
      required,
    },
  ))
}

fn with_executable(selected: &std::process::Command, executable: &Path) -> Command {
  let mut command = Command::new(executable);
  command.args(selected.get_args());
  for (key, value) in selected.get_envs() {
    if let Some(value) = value {
      command.env(key, value);
    } else {
      command.env_remove(key);
    }
  }
  if let Some(directory) = selected.get_current_dir() {
    command.current_dir(directory);
  }
  command
}

fn resolve(selected: &std::process::Command) -> io::Result<PathBuf> {
  let current = std::env::current_dir()?;
  let directory = selected
    .get_current_dir()
    .map_or(current.clone(), |directory| current.join(directory));
  let program = Path::new(selected.get_program());
  if program.is_absolute() || program.components().count() > 1 {
    return directory.join(program).canonicalize();
  }
  let search_path = selected
    .get_envs()
    .find(|(key, _)| *key == "PATH")
    .map_or_else(
      || std::env::var_os("PATH"),
      |(_, value)| value.map(ToOwned::to_owned),
    )
    .unwrap_or_default();
  for candidate_directory in std::env::split_paths(&search_path) {
    let candidate = directory.join(candidate_directory).join(program);
    if is_executable(&candidate) {
      return candidate.canonicalize();
    }
    #[cfg(windows)]
    if candidate.extension().is_none() {
      let candidate = candidate.with_extension("exe");
      if is_executable(&candidate) {
        return candidate.canonicalize();
      }
    }
  }
  Err(io::Error::new(
    io::ErrorKind::NotFound,
    "Helper executable was not found",
  ))
}

fn is_executable(path: &Path) -> bool {
  let Ok(metadata) = path.metadata() else {
    return false;
  };
  if !metadata.is_file() {
    return false;
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
  }
  #[cfg(not(unix))]
  {
    true
  }
}

fn metadata_failure(executable: &Path, required: ProtocolVersion, code: &'static str) -> Error {
  let label = executable_label(executable);
  Error::with_message(
    code,
    format!(
      "Could not verify selected ctld helper {label}; its available ctld_helper contract is unknown, and this operation requires {required} (build {}). Check or rebuild that helper and try again.",
      required.build,
    ),
  )
}

#[allow(
  clippy::unnecessary_debug_formatting,
  reason = "helper paths must escape terminal control characters and preserve non-UTF-8 bytes"
)]
fn executable_label(executable: &Path) -> String {
  format!("{executable:?}")
}
