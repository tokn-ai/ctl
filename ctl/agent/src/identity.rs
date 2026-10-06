//! Account-owned identity stored independently of versioned component bundles.
use ctl_core::component::{ComponentInfo, protocols_match};
use ctl_proto::{BundleVersion, RemoteIdentity};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const MAX_BUNDLE_MANIFEST_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
struct Manifest {
  schema_version: u32,
  #[serde(flatten)]
  version: BundleVersion,
  #[serde(default, deserialize_with = "deserialize_components")]
  components: Option<BTreeMap<String, ComponentInfo>>,
}

fn deserialize_components<'de, D: serde::Deserializer<'de>>(
  deserializer: D,
) -> Result<Option<BTreeMap<String, ComponentInfo>>, D::Error> {
  struct ComponentsVisitor;
  impl<'de> serde::de::Visitor<'de> for ComponentsVisitor {
    type Value = BTreeMap<String, ComponentInfo>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      formatter.write_str("a uniquely named map of the four bundled components")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
      let mut components = BTreeMap::new();
      while let Some((name, component)) = map.next_entry::<String, ComponentInfo>()? {
        if components.insert(name, component).is_some() || components.len() > 4 {
          return Err(serde::de::Error::custom(
            "duplicate or excessive bundle components",
          ));
        }
      }
      Ok(components)
    }
  }
  deserializer.deserialize_map(ComponentsVisitor).map(Some)
}

/// Identifies the installed agent and its persistent per-user environment.
///
/// # Errors
/// Returns an error if identity storage or installed bundle metadata is invalid.
pub fn discover() -> io::Result<RemoteIdentity> {
  discover_at(&data_directory()?, &std::env::current_exe()?)
}

/// Reads an existing account identity without creating files or directories.
///
/// # Errors
/// Returns an error when identity storage is absent or invalid.
pub fn inspect() -> io::Result<RemoteIdentity> {
  inspect_at(&data_directory()?, &std::env::current_exe()?)
}

fn inspect_at(directory: &Path, executable: &Path) -> io::Result<RemoteIdentity> {
  installed_identity(read_id(&directory.join("remote-id"))?, executable)
}

fn data_directory() -> io::Result<PathBuf> {
  ctl_core::paths::directory()
}

fn discover_at(directory: &Path, executable: &Path) -> io::Result<RemoteIdentity> {
  installed_identity(load_or_create_id(directory)?, executable)
}

fn installed_identity(remote_id: String, executable: &Path) -> io::Result<RemoteIdentity> {
  let component = crate::component_info();
  let manifest = executable.with_file_name("manifest.json");
  #[cfg(unix)]
  let managed_bundle =
    match fs::symlink_metadata(executable.with_file_name(ctl_core::bundles::MANIFEST_FILE)) {
      Ok(_) => {
        let bundle =
          ctl_core::bundles::Bundle::open(executable.parent().ok_or_else(invalid_manifest)?)?;
        if !bundle.manifest.same_component("ctl-agent", &component)
          || bundle.manifest.target_triple != ctl_core::paths::native_target()
        {
          return Err(invalid_manifest());
        }
        Some(Box::new(BundleVersion {
          app_version: component.build.version.clone(),
          bundle_id: bundle
            .manifest
            .distribution_id
            .unwrap_or(bundle.manifest.bundle_id),
          git_revision: component
            .build
            .source_revision
            .clone()
            .ok_or_else(invalid_manifest)?,
          target_triple: bundle.manifest.target_triple,
        }))
      }
      Err(error) if error.kind() == io::ErrorKind::NotFound => None,
      Err(error) => return Err(error),
    };
  #[cfg(not(unix))]
  let managed_bundle = None;
  let bundle = if managed_bundle.is_some() {
    managed_bundle
  } else {
    match fs::File::open(manifest) {
      Ok(file) => {
        let mut bytes = Vec::new();
        file
          .take(MAX_BUNDLE_MANIFEST_BYTES as u64 + 1)
          .read_to_end(&mut bytes)?;
        Some(Box::new(parse_manifest(&bytes, &component)?))
      }
      Err(error) if error.kind() == io::ErrorKind::NotFound => None,
      Err(error) => return Err(error),
    }
  };
  let identity = RemoteIdentity {
    remote_id,
    agent_version: component.build.version.clone(),
    build: Some(component.build),
    ctmux_restart_supported: true,
    bundle,
    protocols: component.protocols,
  };
  if !identity.is_valid() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid remote identity",
    ));
  }
  Ok(identity)
}

fn parse_manifest(bytes: &[u8], actual: &ComponentInfo) -> io::Result<BundleVersion> {
  if bytes.len() > MAX_BUNDLE_MANIFEST_BYTES {
    return Err(invalid_manifest());
  }
  let manifest: Manifest = serde_json::from_slice(bytes).map_err(|_| invalid_manifest())?;
  match manifest.schema_version {
    1 if manifest.components.is_none() => {}
    2 => {
      let components = manifest.components.as_ref().ok_or_else(invalid_manifest)?;
      let version = &manifest.version;
      if components.len() != 4
        || ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld"]
          .iter()
          .any(|name| !components.contains_key(*name))
        || version.git_revision.len() != 40
        || !version
          .git_revision
          .bytes()
          .all(|byte| byte.is_ascii_hexdigit())
        || components.values().any(|component| {
          !component.is_valid()
            || component.build.dirty
            || component.build.version != version.app_version
            || component.build.source_revision.as_deref() != Some(&version.git_revision)
        })
      {
        return Err(invalid_manifest());
      }
      for (name, required) in [
        ("ctmuxd", &["ctmux", "ctmux_control"][..]),
        (
          "ctl-taskd",
          &["task", "task_control", "ctmux", "ctmux_control"][..],
        ),
        ("ctld", &["ctld", "ctld_lifecycle", "ctld_helper"][..]),
      ] {
        if required.iter().any(|required| {
          !components[name]
            .protocols
            .iter()
            .any(|protocol| protocol.name == *required)
        }) {
          return Err(invalid_manifest());
        }
      }
      let claimed = &components["ctl-agent"];
      if claimed.build != actual.build || !protocols_match(&claimed.protocols, &actual.protocols) {
        return Err(invalid_manifest());
      }
    }
    _ => return Err(invalid_manifest()),
  }
  Ok(manifest.version)
}

fn invalid_manifest() -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, "invalid agent bundle manifest")
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
  use serde_json::{Value, json};

  fn bundled_component() -> ComponentInfo {
    let mut component = crate::component_info();
    component.build.version = "0.1.0".into();
    component.build.source_revision = Some("a".repeat(40));
    component.build.source_fingerprint = "b".repeat(64);
    component.build.dirty = false;
    component
  }

  fn versioned_manifest(component: &ComponentInfo) -> Value {
    let companion = |protocols| ComponentInfo {
      build: component.build.clone(),
      protocols,
    };
    json!({
      "schema_version": 2,
      "app_version": component.build.version,
      "bundle_id": "0.1.0-dev.aaaaaaaaaaaa",
      "git_revision": component.build.source_revision,
      "target_triple": "aarch64-apple-darwin",
      "components": {
        "ctl-agent": component,
        "ctmuxd": companion(vec![ctmux_proto::protocol_info(), ctmux_ipc::local_control_protocol_info()]),
        "ctl-taskd": companion(vec![ctl_task_proto::protocol_info(), ctl_task_proto::control::protocol_info(), ctmux_proto::protocol_info(), ctmux_ipc::local_control_protocol_info()]),
        "ctld": companion(ctl_ipc::lifecycle::DaemonBinaryInfo::current().protocols),
      }
    })
  }

  #[test]
  fn published_bundle_metadata_matches_the_actual_agent_without_changing_identity() {
    let component = bundled_component();
    let mut manifest = versioned_manifest(&component);
    // A complete protocol map is independent of JSON array ordering.
    manifest["components"]["ctl-agent"]["protocols"]
      .as_array_mut()
      .unwrap()
      .reverse();
    let version = parse_manifest(&serde_json::to_vec(&manifest).unwrap(), &component).unwrap();
    assert_eq!(version.app_version, "0.1.0");
    assert_eq!(version.git_revision, "a".repeat(40));
    assert_eq!(version.bundle_id, "0.1.0-dev.aaaaaaaaaaaa");
    let mut larger = serde_json::to_vec(&manifest).unwrap();
    larger.resize(10 * 1024, b' ');
    assert!(parse_manifest(&larger, &component).is_ok());
  }

  #[test]
  fn published_bundle_metadata_cannot_claim_different_components_or_protocols() {
    let component = bundled_component();
    let valid = versioned_manifest(&component);
    let mut cases = Vec::new();
    let mut missing = valid.clone();
    missing["components"]
      .as_object_mut()
      .unwrap()
      .remove("ctmuxd");
    cases.push(missing);
    let mut extra = valid.clone();
    extra["components"]["other"] = json!(component);
    cases.push(extra);
    let mut dirty = valid.clone();
    dirty["components"]["ctl-taskd"]["build"]["dirty"] = json!(true);
    cases.push(dirty);
    let mut other_source = valid.clone();
    other_source["components"]["ctmuxd"]["build"]["source_revision"] = json!("c".repeat(40));
    cases.push(other_source);
    let mut other_version = valid.clone();
    other_version["components"]["ctl-taskd"]["build"]["version"] = json!("0.2.0");
    cases.push(other_version);
    let mut other_agent = valid.clone();
    other_agent["components"]["ctl-agent"]["build"]["source_fingerprint"] = json!("c".repeat(64));
    cases.push(other_agent);
    let mut missing_protocol = valid.clone();
    missing_protocol["components"]["ctl-agent"]["protocols"]
      .as_array_mut()
      .unwrap()
      .pop();
    cases.push(missing_protocol);
    let mut invalid_protocol = valid.clone();
    invalid_protocol["components"]["ctmuxd"]["protocols"][0]["supported_versions"] = json!([]);
    cases.push(invalid_protocol);
    let mut missing_companion_protocol = valid.clone();
    missing_companion_protocol["components"]["ctl-taskd"]["protocols"]
      .as_array_mut()
      .unwrap()
      .pop();
    cases.push(missing_companion_protocol);
    let mut missing_vpn_daemon = valid.clone();
    missing_vpn_daemon["components"]
      .as_object_mut()
      .unwrap()
      .remove("ctld");
    cases.push(missing_vpn_daemon);
    let mut missing_vpn_daemon_protocol = valid.clone();
    missing_vpn_daemon_protocol["components"]["ctld"]["protocols"]
      .as_array_mut()
      .unwrap()
      .pop();
    cases.push(missing_vpn_daemon_protocol);
    for manifest in cases {
      assert_eq!(
        parse_manifest(&serde_json::to_vec(&manifest).unwrap(), &component)
          .unwrap_err()
          .kind(),
        io::ErrorKind::InvalidData,
      );
    }
  }

  #[test]
  fn unknown_or_oversized_bundle_manifests_are_rejected() {
    let component = bundled_component();
    let mut unknown = versioned_manifest(&component);
    unknown["schema_version"] = json!(3);
    assert!(parse_manifest(&serde_json::to_vec(&unknown).unwrap(), &component).is_err());
    let mut oversized = serde_json::to_vec(&versioned_manifest(&component)).unwrap();
    oversized.resize(MAX_BUNDLE_MANIFEST_BYTES + 1, b' ');
    assert!(parse_manifest(&oversized, &component).is_err());
  }

  #[test]
  fn duplicate_components_and_schema_one_protocol_claims_are_rejected() {
    let component = bundled_component();
    let mut manifest = versioned_manifest(&component);
    let bytes = serde_json::to_string(&manifest).unwrap();
    let duplicate = bytes.replacen(
      "\"components\":{",
      &format!(
        "\"components\":{{\"ctl-agent\":{},",
        serde_json::to_string(&component).unwrap(),
      ),
      1,
    );
    assert!(parse_manifest(duplicate.as_bytes(), &component).is_err());
    manifest["schema_version"] = json!(1);
    assert!(parse_manifest(&serde_json::to_vec(&manifest).unwrap(), &component).is_err());
  }

  #[cfg(unix)]
  #[test]
  fn complete_store_identity_is_pinned_to_verified_bytes_and_the_actual_agent() {
    use ctl_core::bundles::{Manifest as CompleteManifest, Source, Store};
    let home = std::env::temp_dir().join(format!("ctl-managed-identity-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&home).unwrap();
    let component = crate::component_info();
    let versioned = versioned_manifest(&component);
    let components = serde_json::from_value(versioned["components"].clone()).unwrap();
    let files = ctl_core::bundles::COMPONENTS
      .into_iter()
      .map(|name| (name.into(), name.as_bytes().to_vec()))
      .collect();
    let manifest = CompleteManifest::new(
      ctl_core::paths::native_target(),
      Source::Local,
      components,
      &files,
    )
    .unwrap()
    .with_distribution_id("0.1.0-dev.published")
    .unwrap();
    let bundle = Store::new(&home).publish(&manifest, &files).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let executable = bundle.directory.join("ctl-agent");
    let identity = installed_identity(id.clone(), &executable).unwrap();
    assert_eq!(identity.remote_id, id);
    assert_eq!(identity.bundle.unwrap().bundle_id, "0.1.0-dev.published");
    fs::write(bundle.directory.join("ctl-taskd"), b"changed companion").unwrap();
    assert!(installed_identity(id, &executable).is_err());
    fs::remove_dir_all(home).unwrap();
  }

  #[test]
  fn passive_inspection_does_not_create_a_missing_identity() {
    let directory = std::env::temp_dir().join(format!("ctl-inspection-{}", uuid::Uuid::new_v4()));
    assert_eq!(
      inspect_at(&directory, &directory.join("ctl-agent"))
        .unwrap_err()
        .kind(),
      io::ErrorKind::NotFound
    );
    assert!(!directory.exists());
  }

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
      let remote_vpn = identity
        .protocols
        .iter()
        .find(|protocol| protocol.name == "ctl_remote_vpn")
        .unwrap();
      assert_eq!(remote_vpn.version, ctl_ipc::remote_vpn::CONTRACT_V1_0_1);
      assert_eq!(
        remote_vpn.supported_versions,
        [ctl_ipc::remote_vpn::CONTRACT_V1_0_1]
      );
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
