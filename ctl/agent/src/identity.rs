//! Account-owned identity stored independently of versioned component bundles.
use ctl_proto::{BundleVersion, RemoteIdentity};
use serde::Deserialize;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Identifies the installed agent and its persistent per-user environment.
///
/// # Errors
/// Returns an error if identity storage or installed bundle metadata is invalid.
pub fn discover() -> io::Result<RemoteIdentity> {
  discover_at(&data_directory()?, &std::env::current_exe()?)
}

fn data_directory() -> io::Result<PathBuf> {
  dirs::home_dir()
    .map(|home| home.join(".tokn/ctl"))
    .ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::NotFound,
        "remote home directory is unavailable",
      )
    })
}

fn discover_at(directory: &Path, executable: &Path) -> io::Result<RemoteIdentity> {
  let remote_id = load_or_create_id(directory)?;
  let manifest = executable.with_file_name("manifest.json");
  let bundle = match fs::File::open(manifest) {
    Ok(file) => {
      #[derive(Deserialize)]
      struct Manifest {
        schema_version: u32,
        #[serde(flatten)]
        version: BundleVersion,
      }
      let mut bytes = Vec::new();
      file.take(8193).read_to_end(&mut bytes)?;
      let manifest: Manifest = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
      if bytes.len() > 8192 || manifest.schema_version != 1 {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "invalid agent bundle manifest",
        ));
      }
      Some(Box::new(manifest.version))
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
    Err(error) => return Err(error),
  };
  let identity = RemoteIdentity {
    remote_id,
    agent_version: env!("CARGO_PKG_VERSION").into(),
    bundle,
  };
  if !identity.is_valid() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid remote identity",
    ));
  }
  Ok(identity)
}

fn read_id(path: &Path) -> io::Result<String> {
  let mut text = String::new();
  fs::File::open(path)?.take(37).read_to_string(&mut text)?;
  let id = uuid::Uuid::parse_str(&text).map_err(io::Error::other)?;
  if id.is_nil() || id.to_string() != text {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid ctl remote-id file",
    ));
  }
  Ok(text)
}

fn load_or_create_id(directory: &Path) -> io::Result<String> {
  let path = directory.join("remote-id");
  match read_id(&path) {
    Ok(id) => return Ok(id),
    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
    Err(error) => return Err(error),
  }
  fs::create_dir_all(directory)?;
  let id = uuid::Uuid::new_v4().to_string();
  let temporary = directory.join(format!(".remote-id-{id}"));
  let mut options = OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
  }
  let result = (|| {
    let mut file = options.open(&temporary)?;
    file.write_all(id.as_bytes())?;
    file.sync_all()?;
    // Publish a complete file without overwriting another concurrent creator.
    match fs::hard_link(&temporary, &path) {
      Ok(()) => {
        #[cfg(unix)]
        fs::File::open(directory)?.sync_all()?;
        Ok(id)
      }
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => read_id(&path),
      Err(error) => Err(error),
    }
  })();
  let _ = fs::remove_file(temporary);
  result
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn concurrent_connections_and_bundle_upgrades_keep_identity() {
    let directory = std::env::temp_dir().join(format!("ctl-identity-{}", uuid::Uuid::new_v4()));
    let ids: Vec<_> = std::thread::scope(|scope| {
      let workers: Vec<_> = (0..12)
        .map(|_| scope.spawn(|| load_or_create_id(&directory).unwrap()))
        .collect();
      workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect()
    });
    assert!(ids.iter().all(|id| id == &ids[0]));
    for version in ["old", "new"] {
      let executable = directory.join("versions").join(version).join("ctl-agent");
      fs::create_dir_all(executable.parent().unwrap()).unwrap();
      fs::write(executable.with_file_name("manifest.json"), format!(r#"{{"schema_version":1,"app_version":"0.1.0","bundle_id":"{version}","git_revision":"{version}","target_triple":"aarch64-apple-darwin"}}"#)).unwrap();
      let identity = discover_at(&directory, &executable).unwrap();
      assert_eq!(identity.remote_id, ids[0]);
      assert_eq!(identity.bundle.unwrap().bundle_id, version);
    }
    fs::remove_dir_all(directory).unwrap();
  }

  #[test]
  fn separate_environments_differ_and_corrupt_identity_is_never_replaced() {
    let directory = std::env::temp_dir().join(format!("ctl-identity-{}", uuid::Uuid::new_v4()));
    let a = directory.join("a");
    let b = directory.join("b");
    assert_ne!(
      load_or_create_id(&a).unwrap(),
      load_or_create_id(&b).unwrap()
    );
    fs::write(a.join("remote-id"), "invalid").unwrap();
    assert!(load_or_create_id(&a).is_err());
    assert_eq!(fs::read_to_string(a.join("remote-id")).unwrap(), "invalid");
    fs::remove_dir_all(directory).unwrap();
  }
}
