use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use ctld_ipc::{VpnConnection, VpnProvider, VpnSettings, VpnSnapshot, VpnState};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use super::{
  DeleteVpnConnectionRequest, SaveVpnConnectionRequest, VpnConnectionsSnapshot, VpnSettingsInput,
};
use crate::error::{CommandErrorDto, CommandResult};

const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 128;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
  schema_version: u32,
  connections: Vec<VpnConnection>,
}

impl Default for Document {
  fn default() -> Self {
    Self {
      schema_version: 2,
      connections: Vec::new(),
    }
  }
}

impl Document {
  fn validate(&self) -> CommandResult<()> {
    if !matches!(self.schema_version, 1 | 2) {
      return Err(error(
        "vpn_version_unsupported",
        "This VPN settings version is not supported.",
      ));
    }
    if self.connections.len() > MAX_CONNECTIONS {
      return Err(invalid("Too many saved VPN connections."));
    }
    let mut ids = HashSet::new();
    for connection in &self.connections {
      if self.schema_version == 1 && connection.provider() != VpnProvider::Openconnect {
        return Err(invalid(
          "Tailscale connections require VPN settings version 2.",
        ));
      }
      connection.validate().map_err(invalid)?;
      if !ids.insert(&connection.connection_id) {
        return Err(invalid("VPN connection IDs must be unique."));
      }
    }
    Ok(())
  }
}

struct Loaded {
  revision: Option<String>,
  document: Document,
}

impl Loaded {
  fn snapshot(&self) -> VpnConnectionsSnapshot {
    VpnConnectionsSnapshot {
      revision: self.revision.clone(),
      connections: self.document.connections.iter().map(Into::into).collect(),
    }
  }
}

pub(super) struct Repository {
  directory: PathBuf,
}

impl Repository {
  pub(super) fn new(directory: PathBuf) -> Self {
    Self { directory }
  }

  pub(super) fn load(&self) -> CommandResult<VpnConnectionsSnapshot> {
    let _lock = self.lock()?;
    Ok(self.read()?.snapshot())
  }

  pub(super) fn connection(&self, connection_id: &str) -> CommandResult<VpnConnection> {
    let _lock = self.lock()?;
    self
      .read()?
      .document
      .connections
      .into_iter()
      .find(|connection| connection.connection_id == connection_id)
      .ok_or_else(not_found)
  }

  pub(super) fn contains(&self, connection_id: &str) -> CommandResult<bool> {
    let _lock = self.lock()?;
    Ok(
      self
        .read()?
        .document
        .connections
        .iter()
        .any(|connection| connection.connection_id == connection_id),
    )
  }

  /// Only enrollment may create an already-connected profile; existing profile
  /// editing continues to require a disconnected runtime.
  pub(super) fn save_enrollment(
    &self,
    expected_revision: Option<&str>,
    connection: VpnConnection,
  ) -> CommandResult<VpnConnectionsSnapshot> {
    if connection.provider() != VpnProvider::Tailscale {
      return Err(invalid("Only Tailscale supports browser enrollment."));
    }
    connection.validate().map_err(invalid)?;
    let _lock = self.lock()?;
    let mut current = self.read()?;
    if let Some(existing) = current
      .document
      .connections
      .iter()
      .find(|existing| existing.connection_id == connection.connection_id)
    {
      // A retry after a successful write must not overwrite later edits, and
      // must also work when the caller missed the original successful reply.
      return if existing == &connection {
        Ok(current.snapshot())
      } else {
        Err(error(
          "vpn_connections_conflict",
          "This connection changed on disk. Reload before saving.",
        ))
      };
    }
    check_revision(current.revision.as_deref(), expected_revision)?;
    current.document.connections.push(connection);
    self.persist(current.document)
  }

  #[cfg(test)]
  pub(super) fn save(
    &self,
    request: SaveVpnConnectionRequest,
  ) -> CommandResult<VpnConnectionsSnapshot> {
    self.save_with_status(request, &VpnSnapshot::default())
  }

  pub(super) fn save_with_status(
    &self,
    request: SaveVpnConnectionRequest,
    status: &VpnSnapshot,
  ) -> CommandResult<VpnConnectionsSnapshot> {
    require_disconnected(&request.connection.connection_id, status)?;
    let _lock = self.lock()?;
    let mut current = self.read()?;
    check_revision(
      current.revision.as_deref(),
      request.expected_revision.as_deref(),
    )?;
    let input = request.connection;
    let existing = current
      .document
      .connections
      .iter()
      .position(|connection| connection.connection_id == input.connection_id);
    let settings = match input.settings {
      VpnSettingsInput::Openconnect {
        url,
        username,
        password,
        auth_method,
        target_ip,
      } => {
        let password = password
          .or_else(|| {
            existing.and_then(
              |index| match &current.document.connections[index].settings {
                VpnSettings::Openconnect { password, .. } => Some(password.clone()),
                VpnSettings::Tailscale { .. } => None,
              },
            )
          })
          .ok_or_else(|| invalid("A new OpenConnect connection requires a password."))?;
        VpnSettings::Openconnect {
          url,
          username,
          password,
          auth_method,
          target_ip,
        }
      }
      VpnSettingsInput::Tailscale {
        hostname,
        accept_routes,
      } => VpnSettings::Tailscale {
        hostname,
        accept_routes,
      },
    };
    let connection = VpnConnection {
      connection_id: input.connection_id,
      name: input.name,
      settings,
    };
    connection.validate().map_err(invalid)?;
    if let Some(index) = existing {
      current.document.connections[index] = connection;
    } else {
      current.document.connections.push(connection);
    }
    self.persist(current.document)
  }

  pub(super) fn delete(
    &self,
    request: &DeleteVpnConnectionRequest,
    status: &VpnSnapshot,
  ) -> CommandResult<VpnConnectionsSnapshot> {
    require_disconnected(&request.connection_id, status)?;
    let _lock = self.lock()?;
    let mut current = self.read()?;
    check_revision(
      current.revision.as_deref(),
      request.expected_revision.as_deref(),
    )?;
    let Some(index) = current
      .document
      .connections
      .iter()
      .position(|connection| connection.connection_id == request.connection_id)
    else {
      return Err(not_found());
    };
    current.document.connections.remove(index);
    self.persist(current.document)
  }

  fn read(&self) -> CommandResult<Loaded> {
    let path = self.directory.join("vpns.json");
    regular_or_absent(&path)?;
    let file = match private_options().read(true).open(path) {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        return Ok(Loaded {
          revision: None,
          document: Document::default(),
        });
      }
      Err(error) => return Err(io_error(error)),
    };
    require_private_file(&file)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file
      .take(MAX_BYTES + 1)
      .read_to_end(&mut bytes)
      .map_err(io_error)?;
    if bytes.len() as u64 > MAX_BYTES {
      return Err(invalid("Saved VPN connections exceed the size limit."));
    }
    // Parser details may quote invalid JSON values, including a password.
    let document: Document = serde_json::from_slice(&bytes)
      .map_err(|_| invalid("Could not read vpns.json; the file has been preserved."))?;
    document.validate()?;
    Ok(Loaded {
      revision: Some(revision(&bytes)),
      document,
    })
  }

  fn lock(&self) -> CommandResult<File> {
    match fs::symlink_metadata(&self.directory) {
      Ok(metadata) if metadata.is_dir() => {}
      Ok(_) => {
        return Err(invalid(
          "The VPN settings directory must not be a symlink or file.",
        ));
      }
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
          use std::os::unix::fs::DirBuilderExt as _;
          builder.mode(0o700);
        }
        builder.create(&self.directory).map_err(io_error)?;
      }
      Err(error) => return Err(io_error(error)),
    }
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    }
    let path = self.directory.join("vpns.lock");
    regular_or_absent(&path)?;
    let file = private_options()
      .create(true)
      .read(true)
      .write(true)
      .open(path)
      .map_err(io_error)?;
    require_private_file(&file)?;
    file.lock().map_err(io_error)?;
    Ok(file)
  }

  fn persist(&self, mut document: Document) -> CommandResult<VpnConnectionsSnapshot> {
    // Reading a legacy document preserves its bytes and revision. Only a saved
    // change migrates the format, after the usual optimistic revision check.
    document.schema_version = 2;
    document.validate()?;
    let bytes = Zeroizing::new(
      serde_json::to_vec_pretty(&document)
        .map_err(|_| invalid("Could not encode the VPN settings."))?,
    );
    if bytes.len() as u64 > MAX_BYTES {
      return Err(invalid("Saved VPN connections exceed the size limit."));
    }
    let temporary = TemporaryFile(
      self
        .directory
        .join(format!(".vpns-{}.tmp", uuid::Uuid::new_v4())),
    );
    let mut file = private_options()
      .create_new(true)
      .write(true)
      .open(&temporary.0)
      .map_err(io_error)?;
    file.write_all(&bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    drop(file);
    regular_or_absent(&self.directory.join("vpns.json"))?;
    fs::rename(&temporary.0, self.directory.join("vpns.json")).map_err(io_error)?;
    #[cfg(unix)]
    File::open(&self.directory)
      .and_then(|file| file.sync_all())
      .map_err(io_error)?;
    Ok(
      Loaded {
        revision: Some(revision(&bytes)),
        document,
      }
      .snapshot(),
    )
  }
}

fn private_options() -> OpenOptions {
  let mut options = OpenOptions::new();
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600).custom_flags(
      i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).expect("O_NOFOLLOW fits an OS flag"),
    );
  }
  #[cfg(windows)]
  {
    use std::os::windows::fs::OpenOptionsExt as _;
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
  }
  options
}

fn require_private_file(file: &File) -> CommandResult<()> {
  let metadata = file.metadata().map_err(io_error)?;
  if !metadata.is_file() {
    return Err(invalid(
      "VPN settings must be regular files, not symlinks or directories.",
    ));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o077 != 0 {
      return Err(error(
        "vpn_storage_permissions",
        "VPN settings files must be private (mode 0600).",
      ));
    }
  }
  Ok(())
}

fn regular_or_absent(path: &Path) -> CommandResult<()> {
  match fs::symlink_metadata(path) {
    Ok(metadata) if metadata.is_file() => Ok(()),
    Ok(_) => Err(invalid(
      "VPN settings must be regular files, not symlinks or directories.",
    )),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(io_error(error)),
  }
}

fn revision(bytes: &[u8]) -> String {
  format!("sha256:{:x}", Sha256::digest(bytes))
}

fn check_revision(current: Option<&str>, expected: Option<&str>) -> CommandResult<()> {
  if current != expected {
    return Err(error(
      "vpn_connections_conflict",
      "VPN connections changed on disk. Reload them before saving.",
    ));
  }
  Ok(())
}

fn not_found() -> CommandErrorDto {
  error(
    "vpn_connection_not_found",
    "The saved VPN connection no longer exists.",
  )
}

fn require_disconnected(connection_id: &str, status: &VpnSnapshot) -> CommandResult<()> {
  if status.connections.iter().any(|connection| {
    connection.connection_id.as_deref() == Some(connection_id)
      && connection.state != VpnState::Stopped
  }) {
    return Err(error(
      "vpn_connection_active",
      "Disconnect this VPN before changing its saved connection.",
    ));
  }
  Ok(())
}

fn invalid(message: impl Into<String>) -> CommandErrorDto {
  error("vpn_connections_invalid", message)
}

fn error(code: &str, message: impl Into<String>) -> CommandErrorDto {
  CommandErrorDto::new(code, message)
}

fn io_error(_: io::Error) -> CommandErrorDto {
  error(
    "vpn_storage_io",
    "Could not access the saved VPN connections.",
  )
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
  }
}
