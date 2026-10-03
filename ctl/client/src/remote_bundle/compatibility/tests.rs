use super::*;
use crate::remote_bundle::tests::{REVISION, TARGET, VERSION};
use crate::remote_bundle::{BundleSet, SUPPORTED_TARGETS, VerifiedBundle};
use ctl_core::component::{ComponentBuildInfo, ProtocolInfo};
use serde_json::{Value, json};

pub(crate) struct Fixture {
  pub outer: Value,
  pub inner: Value,
  pub files: BTreeMap<String, Vec<u8>>,
}

impl Fixture {
  pub(crate) fn new(version: &str, revision: &str) -> Self {
    let build = ComponentBuildInfo {
      version: version.into(),
      source_revision: Some(revision.into()),
      source_fingerprint: "a".repeat(64),
      dirty: false,
    };
    let ctmux = vec![
      ctmux_proto::protocol_info(),
      ctmux_ipc::local_control_protocol_info(),
    ];
    let task = vec![
      ctl_task_proto::protocol_info(),
      ctl_task_proto::control::protocol_info(),
    ];
    let mut agent = ctl_proto::agent_protocols();
    agent.extend(ctmux.clone());
    agent.extend(task.clone());
    let mut task_dependencies = task;
    task_dependencies.extend(ctmux.clone());
    let components = BTreeMap::from([
      (
        "ctl-agent",
        ComponentInfo {
          build: build.clone(),
          protocols: agent,
        },
      ),
      (
        "ctmuxd",
        ComponentInfo {
          build: build.clone(),
          protocols: ctmux,
        },
      ),
      (
        "ctl-taskd",
        ComponentInfo {
          build,
          protocols: task_dependencies,
        },
      ),
    ]);
    let files: BTreeMap<_, _> = COMPONENTS
      .iter()
      .map(|name| {
        (
          (*name).into(),
          format!("binary fixture {name}").into_bytes(),
        )
      })
      .collect();
    let hashes: BTreeMap<_, _> = files
      .iter()
      .map(|(name, bytes)| (name, format!("{:x}", Sha256::digest(bytes))))
      .collect();
    let inner = json!({"schema_version":2,"app_version":version,"bundle_id":version,"git_revision":revision,"target_triple":TARGET,"files":hashes,"components":components});
    let targets: BTreeMap<_, _> = SUPPORTED_TARGETS.iter().map(|target| (*target, json!({"archive":format!("ctl-agent-bundle-{version}-{target}.tar.gz"),"sha256":"a".repeat(64),"components":components}))).collect();
    let outer = json!({"schema_version":2,"app_version":version,"bundle_id":version,"git_revision":revision,"targets":targets});
    Self {
      outer,
      inner,
      files,
    }
  }

  pub(crate) fn archive(&self) -> Vec<u8> {
    self.archive_with(None)
  }

  fn archive_with(&self, extra: Option<(&str, tar::EntryType)>) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (name, bytes) in &self.files {
      append(&mut builder, name, bytes, tar::EntryType::Regular);
    }
    append(
      &mut builder,
      "manifest.json",
      &serde_json::to_vec(&self.inner).unwrap(),
      tar::EntryType::Regular,
    );
    if let Some((name, kind)) = extra {
      append(&mut builder, name, b"", kind);
    }
    builder.into_inner().unwrap().finish().unwrap()
  }

  pub(crate) fn bind_archive(&self, archive: &[u8]) -> Vec<u8> {
    let mut outer = self.outer.clone();
    for target in SUPPORTED_TARGETS {
      outer["targets"][target]["sha256"] = json!(format!("{:x}", Sha256::digest(archive)));
    }
    serde_json::to_vec(&outer).unwrap()
  }

  pub(crate) fn bundle(&self) -> VerifiedBundle {
    let archive = self.archive();
    let bytes = self.bind_archive(&archive);
    BundleSet::parse_intrinsic(&bytes)
      .unwrap()
      .verify_archive(TARGET, archive, bytes)
      .unwrap()
  }

  fn verify(&self) -> Result<VerifiedBundle, Error> {
    let archive = self.archive();
    let bytes = self.bind_archive(&archive);
    BundleSet::parse_intrinsic(&bytes)?.verify_archive(TARGET, archive, bytes)
  }
}

fn append<W: std::io::Write>(
  builder: &mut tar::Builder<W>,
  name: &str,
  bytes: &[u8],
  kind: tar::EntryType,
) {
  let mut header = tar::Header::new_ustar();
  header.set_mode(0o700);
  header.set_size(bytes.len() as u64);
  header.set_entry_type(kind);
  // Raw headers deliberately preserve representative unsafe path fixtures.
  header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
  header.set_cksum();
  builder.append(&header, bytes).unwrap();
}

#[test]
fn schema2_binds_actual_binary_hashes_and_each_component_advertisement() {
  let fixture = Fixture::new(VERSION, REVISION);
  let bundle = fixture.bundle();
  assert!(
    BundleSet::parse_intrinsic(&bundle.manifest)
      .unwrap()
      .is_compatible(TARGET)
      .unwrap()
  );
  for change in 0..6 {
    let mut fixture = Fixture::new(VERSION, REVISION);
    match change {
      0 => fixture.files.get_mut("ctmuxd").unwrap().push(1),
      1 => fixture.inner["git_revision"] = json!("b".repeat(40)),
      2 => fixture.inner["target_triple"] = json!("x86_64-unknown-linux-musl"),
      3 => fixture.inner["components"]["ctl-agent"]["protocols"][0]["build"] = json!(4),
      4 => fixture.inner["files"]["extra"] = json!("a".repeat(64)),
      _ => {
        fixture.files.remove("ctl-taskd");
      }
    }
    assert!(
      matches!(fixture.verify(), Err(Error::Invalid(_))),
      "change {change}"
    );
  }
}

#[test]
fn schema2_provenance_and_advertisement_maps_are_intrinsically_validated() {
  for change in 0..8 {
    let mut fixture = Fixture::new(VERSION, REVISION);
    let component = &mut fixture.outer["targets"][TARGET]["components"]["ctl-agent"];
    match change {
      0 => component["build"]["dirty"] = json!(true),
      1 => component["build"]["version"] = json!("different"),
      2 => component["build"]["source_revision"] = json!("b".repeat(40)),
      3 => component["protocols"][0]["version"] = json!(3),
      4 => component["protocols"][0]["supported_versions"] = json!([]),
      5 => component["protocols"]
        .as_array_mut()
        .unwrap()
        .pop()
        .map(|_| ())
        .unwrap(),
      6 => {
        fixture.outer["targets"][TARGET]["components"]
          .as_object_mut()
          .unwrap()
          .remove("ctmuxd");
      }
      _ => fixture.outer["targets"][TARGET]["components"]["foreign"] = component.clone(),
    }
    assert!(
      BundleSet::parse_intrinsic(&serde_json::to_vec(&fixture.outer).unwrap()).is_err(),
      "change {change}"
    );
  }
}

#[test]
fn archive_validation_rejects_duplicate_paths_links_extensions_and_traversal() {
  let fixture = Fixture::new(VERSION, REVISION);
  for (name, kind) in [
    ("ctmuxd", tar::EntryType::Regular),
    ("linked", tar::EntryType::Symlink),
    ("../outside", tar::EntryType::Regular),
    ("./ctmuxd", tar::EntryType::Regular),
    ("pax", tar::EntryType::XGlobalHeader),
    ("fifo", tar::EntryType::Fifo),
  ] {
    let archive = fixture.archive_with(Some((name, kind)));
    let bytes = fixture.bind_archive(&archive);
    assert!(
      BundleSet::parse_intrinsic(&bytes)
        .unwrap()
        .verify_archive(TARGET, archive, bytes)
        .is_err()
    );
  }
}

fn compress_tar(tar: &[u8]) -> Vec<u8> {
  use std::io::Write as _;
  let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
  encoder.write_all(tar).unwrap();
  encoder.finish().unwrap()
}

#[test]
fn archive_validation_requires_complete_ends_crc_and_bounded_executable_binaries() {
  let fixture = Fixture::new(VERSION, REVISION);
  let mut tar = Vec::new();
  flate2::read::GzDecoder::new(fixture.archive().as_slice())
    .read_to_end(&mut tar)
    .unwrap();
  let verify = |archive: Vec<u8>| {
    let bytes = fixture.bind_archive(&archive);
    BundleSet::parse_intrinsic(&bytes)
      .unwrap()
      .verify_archive(TARGET, archive, bytes)
  };
  for end_bytes in [0, 512, 1023] {
    assert!(verify(compress_tar(&tar[..tar.len() - 1024 + end_bytes])).is_err());
  }
  let mut hidden = tar.clone();
  hidden.extend_from_slice(&tar[..512]);
  assert!(verify(compress_tar(&hidden)).is_err());
  let mut truncated = fixture.archive();
  truncated.pop();
  assert!(verify(truncated).is_err());
  for change in 0..3 {
    let mut changed = tar.clone();
    let mut header = tar::Header::new_ustar();
    header.as_mut_bytes().copy_from_slice(&changed[..512]);
    match change {
      0 => header.set_mode(0o600),
      1 => header.set_size(0),
      _ => header.set_size(MAX_BINARY_BYTES + 1),
    }
    header.set_cksum();
    changed[..512].copy_from_slice(header.as_bytes());
    assert!(
      verify(compress_tar(&changed)).is_err(),
      "header change {change}"
    );
  }
}

#[test]
fn duplicate_component_keys_and_schema1_advertisements_are_rejected() {
  let fixture = Fixture::new(VERSION, REVISION);
  let serialized = serde_json::to_string(&fixture.outer).unwrap();
  let component =
    serde_json::to_string(&fixture.outer["targets"][TARGET]["components"]["ctmuxd"]).unwrap();
  let duplicated = serialized.replacen(
    "\"components\":{",
    &format!("\"components\":{{\"ctmuxd\":{component},"),
    1,
  );
  assert!(BundleSet::parse_intrinsic(duplicated.as_bytes()).is_err());
  for schema in [1, 2] {
    let mut changed = fixture.outer.clone();
    changed["schema_version"] = json!(schema);
    changed["targets"][TARGET]["components"] = json!(null);
    assert!(BundleSet::parse_intrinsic(&serde_json::to_vec(&changed).unwrap()).is_err());
  }
  let mut changed = fixture.outer;
  changed["schema_version"] = json!(1);
  assert!(BundleSet::parse_intrinsic(&serde_json::to_vec(&changed).unwrap()).is_err());
}

#[test]
fn supported_version_and_protocol_map_order_do_not_change_archive_identity() {
  let mut fixture = Fixture::new(VERSION, REVISION);
  for target in SUPPORTED_TARGETS {
    for component in fixture.outer["targets"][target]["components"]
      .as_object_mut()
      .unwrap()
      .values_mut()
    {
      for protocol in component["protocols"].as_array_mut().unwrap() {
        let old: ProtocolVersion = serde_json::from_value(protocol["version"].clone()).unwrap();
        let new = ProtocolVersion::new(old.major, old.minor + 1, old.build + 2);
        protocol["build"] = json!(new.build);
        protocol["version"] = json!(new);
        protocol["supported_versions"] = json!([old, new]);
      }
    }
  }
  fixture.inner["components"] = fixture.outer["targets"][TARGET]["components"].clone();
  for component in fixture.inner["components"]
    .as_object_mut()
    .unwrap()
    .values_mut()
  {
    let protocols = component["protocols"].as_array_mut().unwrap();
    protocols.reverse();
    for protocol in protocols {
      protocol["supported_versions"]
        .as_array_mut()
        .unwrap()
        .reverse();
    }
  }
  fixture.bundle();
}

#[test]
fn client_and_every_internal_companion_contract_must_explicitly_intersect() {
  let fixture = Fixture::new("0.0.9", &"b".repeat(40));
  let parsed = BundleSet::parse_intrinsic(&serde_json::to_vec(&fixture.outer).unwrap()).unwrap();
  assert!(parsed.is_compatible(TARGET).unwrap());
  for component in COMPONENTS {
    let baseline = parsed.target(TARGET).unwrap().components.as_ref().unwrap();
    for index in 0..baseline[component].protocols.len() {
      let mut changed = baseline.clone();
      let protocol = &mut changed.get_mut(component).unwrap().protocols[index];
      let different = ProtocolVersion::new(2, 0, protocol.build);
      *protocol = ProtocolInfo::new(&protocol.name, protocol.build, different, &[different]);
      assert!(!compatible(&changed), "{component} protocol {index}");
    }
  }
}

#[test]
fn newer_explicit_contracts_retain_older_support_without_inferred_build_gaps() {
  let fixture = Fixture::new(VERSION, REVISION);
  let parsed = BundleSet::parse_intrinsic(&serde_json::to_vec(&fixture.outer).unwrap()).unwrap();
  let mut components = parsed.target(TARGET).unwrap().components.clone().unwrap();
  for component in components.values_mut() {
    for protocol in &mut component.protocols {
      let old = protocol.version;
      let new = ProtocolVersion::new(old.major, old.minor + 1, old.build + 2);
      *protocol = ProtocolInfo::new(&protocol.name, new.build + 1, new, &[old, new]);
    }
  }
  assert!(compatible(&components));
  for component in COMPONENTS {
    let mut future_only = components.clone();
    for protocol in &mut future_only.get_mut(component).unwrap().protocols {
      protocol.supported_versions = vec![protocol.version];
    }
    assert!(!compatible(&future_only));
  }
}
