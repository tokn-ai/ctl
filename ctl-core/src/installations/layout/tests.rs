use super::*;
use crate::bundle_layout::{ALL, executable_name, required_protocols};
use crate::bundles::Source;
use crate::component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo};
use crate::installations::{CatalogEntry, CatalogOrigin};
use crate::protocol::ProtocolVersion;

const TARGET: &str = "aarch64-apple-darwin";

struct TestStore(PathBuf);

impl TestStore {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-layout-{}", uuid::Uuid::new_v4()));
    crate::private_fs::create_directory(&path).unwrap();
    Self(path)
  }

  fn store(&self) -> Installations {
    Installations::new(&self.0)
  }

  fn root(&self) -> PathBuf {
    self.store().root.join("installs")
  }

  fn fixture(&self, version: &str, distribution: Option<&str>, byte: u8) -> String {
    let build = ComponentBuildInfo {
      version: version.into(),
      source_revision: Some("a".repeat(40)),
      source_fingerprint: "b".repeat(64),
      dirty: false,
    };
    let contract = ProtocolVersion::new(1, 0, 1);
    let components = ALL
      .into_iter()
      .map(|name| {
        let protocols = required_protocols(name)
          .iter()
          .map(|&name| ProtocolInfo::new(name, 1, contract, &[contract]))
          .collect();
        (
          name.into(),
          ComponentInfo {
            build: build.clone(),
            protocols,
          },
        )
      })
      .collect();
    let files = ALL
      .into_iter()
      .map(|name| (executable_name(TARGET, name).into_owned(), vec![byte]))
      .collect();
    let mut manifest = Manifest::new(TARGET, Source::Ci, components, &files).unwrap();
    if let Some(distribution) = distribution {
      manifest = manifest.with_distribution_id(distribution).unwrap();
    }
    let bundle = crate::bundles::Store::cache(&self.0)
      .publish(&manifest, &files)
      .unwrap();
    let store = self.store();
    filesystem::ensure_root(&store).unwrap();
    filesystem::publish(&store, &bundle, Some(&files)).unwrap();
    manifest.bundle_id
  }
}

impl Drop for TestStore {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn entry(id: &str, channel: &str, publication: Option<&str>, current: bool) -> CatalogEntry {
  CatalogEntry {
    bundle_id: id.into(),
    target_triple: TARGET.into(),
    origin: Some(CatalogOrigin {
      channel: channel.into(),
      publication_id: publication.map(str::to_owned),
      built_at: None,
    }),
    current,
  }
}

#[test]
fn groups_versions_and_checkout_heads_without_copying_payloads() {
  let home = TestStore::new();
  let nightly = "0.1.2-nightly.20261110+gaaaaaaaaaaaa";
  let id = home.fixture("0.1.2", Some(nightly), 1);
  let checkout = "a".repeat(64);
  let store = home.store();
  let mut state = State::default();
  state.entries = vec![
    entry(&id, "stable", Some("0.1.2-1-1"), true),
    entry(&id, "nightly", Some(&format!("{nightly}-1-1")), true),
    entry(&id, "pr-7", Some("test-pr-7"), true),
    entry(&id, "provided", None, true),
    entry(&id, "local", None, true),
  ];
  state.development.insert(checkout.clone(), id.clone());
  sync(&store, &state).unwrap();
  let root = home.root();
  for path in [
    format!("stable/0.1.2/{TARGET}"),
    format!("nightly/{nightly}/{TARGET}"),
    format!("dev/pr-7/0.1.2/{TARGET}"),
    format!("dev/provided/0.1.2/{TARGET}"),
    format!("dev/{checkout}"),
  ] {
    assert_eq!(read_reference(&root, &path).unwrap().as_ref(), Some(&id));
    #[cfg(unix)]
    assert_eq!(
      reference_path(&root, &path).canonicalize().unwrap(),
      root.join(&id).canonicalize().unwrap()
    );
  }
  assert!(!root.join("dev/unassigned").exists());
  assert_eq!(filesystem::entries(&store).unwrap().len(), 1);
}

#[test]
fn refresh_replaces_owned_refs_and_preserves_unrelated_paths() {
  let home = TestStore::new();
  let first = home.fixture("0.1.2", None, 1);
  let second = home.fixture("0.1.2", None, 2);
  let root = home.root();
  let store = home.store();
  let mut state = State::default();
  let path = format!("stable/0.1.2/{TARGET}");
  state.entries = vec![entry(&first, "stable", None, true)];
  sync(&store, &state).unwrap();

  state.entries[0].current = false;
  state.entries.push(entry(&second, "stable", None, true));
  sync(&store, &state).unwrap();
  assert_eq!(read_reference(&root, &path).unwrap(), Some(second));

  let version = root.join("stable/0.1.2");
  let note = version.join("notes.txt");
  fs::write(&note, b"owned by the account").unwrap();
  state.entries.clear();
  sync(&store, &state).unwrap();
  assert!(read_reference(&root, &path).unwrap().is_none());
  assert_eq!(fs::read(&note).unwrap(), b"owned by the account");
  assert!(root.join(first).is_dir());

  fs::write(reference_path(&root, &path), b"user file").unwrap();
  state
    .entries
    .push(entry(&home.fixture("0.1.2", None, 3), "stable", None, true));
  assert!(sync(&store, &state).is_err());
  assert_eq!(
    fs::read(reference_path(&root, &path)).unwrap(),
    b"user file"
  );
}

#[test]
fn retries_after_references_were_updated_before_recording_completion() {
  let home = TestStore::new();
  let first = home.fixture("0.1.2", None, 1);
  let second = home.fixture("0.1.2", None, 2);
  let path = format!("stable/0.1.2/{TARGET}");
  let root = home.root();
  let store = home.store();
  let mut state = State::default();
  state.entries.push(entry(&first, "stable", None, true));
  sync(&store, &state).unwrap();
  write_json(
    &root.join(SNAPSHOT),
    &Snapshot {
      previous: References::from([(path.clone(), first)]),
      pending: Some(References::from([(path.clone(), second.clone())])),
    },
    MAX_SNAPSHOT_BYTES,
  )
  .unwrap();
  update_reference(&root, &path, &second).unwrap();
  state.entries = vec![entry(&second, "stable", None, true)];
  sync(&store, &state).unwrap();
  let snapshot: Snapshot = read_json(&root.join(SNAPSHOT), MAX_SNAPSHOT_BYTES)
    .unwrap()
    .unwrap();
  assert!(snapshot.pending.is_none());
  assert_eq!(snapshot.previous.get(&path), Some(&second));
}

#[test]
fn invalid_labels_are_safely_named_and_case_sensitive_versions_distinguish_on_disk() {
  let home = TestStore::new();
  let upper = home.fixture("0.1.2+ALPHA", None, 1);
  let lower = home.fixture("0.1.2+alpha", None, 2);
  let invalid = home.fixture("../../notes", None, 3);
  let mut state = State::default();
  state.entries = vec![
    entry(&upper, "provided", None, false),
    entry(&lower, "provided", None, true),
    entry(&invalid, "provided", None, false),
  ];
  let paths = references(&home.root(), &state).unwrap();
  assert_eq!(paths.len(), 3);
  assert_eq!(
    paths[&format!("dev/provided/0.1.2+_41_4c_50_48_41/{TARGET}")],
    upper
  );
  assert_eq!(paths[&format!("dev/provided/0.1.2+alpha/{TARGET}")], lower);
  assert_eq!(
    paths[&format!("dev/provided/build-{invalid}/{TARGET}")],
    invalid
  );
}

#[cfg(unix)]
#[test]
fn refuses_to_follow_an_existing_channel_symlink() {
  let home = TestStore::new();
  let id = home.fixture("0.1.2", None, 1);
  let root = home.root();
  let elsewhere = root.join("elsewhere");
  filesystem::ensure_directory(&elsewhere).unwrap();
  std::os::unix::fs::symlink(&elsewhere, root.join("stable")).unwrap();
  let mut state = State::default();
  state.entries.push(entry(&id, "stable", None, true));
  assert!(sync(&home.store(), &state).is_err());
  assert_eq!(fs::read_dir(elsewhere).unwrap().count(), 0);
}
