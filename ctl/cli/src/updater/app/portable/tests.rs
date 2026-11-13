use super::*;
use std::io::{Cursor, Write as _};
use zip::write::SimpleFileOptions;

const X64: &str = "x86_64-pc-windows-msvc";
const ARM64: &str = "aarch64-pc-windows-msvc";

struct Scratch(PathBuf);

impl Scratch {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("ctlup-portable-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    Self(root)
  }
}

impl Drop for Scratch {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn pe(machine: u16) -> Vec<u8> {
  let mut bytes = vec![0; 240];
  bytes[..2].copy_from_slice(b"MZ");
  bytes[60..64].copy_from_slice(&64_u32.to_le_bytes());
  bytes[64..68].copy_from_slice(b"PE\0\0");
  bytes[68..70].copy_from_slice(&machine.to_le_bytes());
  bytes[70..72].copy_from_slice(&1_u16.to_le_bytes());
  bytes[84..86].copy_from_slice(&112_u16.to_le_bytes());
  bytes[86..88].copy_from_slice(&2_u16.to_le_bytes());
  bytes[88..90].copy_from_slice(&0x20b_u16.to_le_bytes());
  bytes
}

fn files(target: &str) -> BTreeMap<String, Vec<u8>> {
  let mut files: BTreeMap<_, _> = EXECUTABLES
    .into_iter()
    .map(|name| (name.to_owned(), pe(machine(target).unwrap())))
    .collect();
  files.insert(
    "resources/agent-bundles/manifest.json".into(),
    b"resource data".to_vec(),
  );
  files
}

fn package(root: &Path, target: &str, mut files: BTreeMap<String, Vec<u8>>) -> PathBuf {
  let hashes: BTreeMap<_, _> = files
    .iter()
    .map(|(name, bytes)| (name, format!("{:x}", Sha256::digest(bytes))))
    .collect();
  let manifest = serde_json::to_vec(&serde_json::json!({
    "schema_version": 1, "app_version": "0.1.3", "target_triple": target, "files": hashes,
  }))
  .unwrap();
  files.insert(MANIFEST.into(), manifest);
  let path = root.join("package.zip");
  let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
  for (name, bytes) in files {
    zip
      .start_file(format!("ctmux/{name}"), SimpleFileOptions::default())
      .unwrap();
    zip.write_all(&bytes).unwrap();
  }
  zip.finish().unwrap();
  path
}

#[test]
fn stages_both_native_architectures_and_checks_the_complete_inventory() {
  for target in [X64, ARM64] {
    let scratch = Scratch::new();
    let archive = package(&scratch.0, target, files(target));
    let staging = scratch.0.join("stage");
    fs::create_dir(&staging).unwrap();
    let app = stage(&archive, &staging, "0.1.3", target).unwrap();
    verify(&app, "0.1.3", target).unwrap();
    assert_eq!(
      fs::read(app.join("ctld.exe")).unwrap(),
      pe(machine(target).unwrap())
    );
    fs::write(app.join("unexpected.txt"), b"unlisted").unwrap();
    assert!(verify(&app, "0.1.3", target).is_err());
  }
}

#[test]
fn rejects_foreign_images_even_with_matching_hashes() {
  let scratch = Scratch::new();
  let mut payload = files(X64);
  payload.insert("ctld.exe".into(), pe(0xaa64));
  let archive = package(&scratch.0, X64, payload);
  let staging = scratch.0.join("stage");
  fs::create_dir(&staging).unwrap();
  assert!(stage(&archive, &staging, "0.1.3", X64).is_err());
}

#[test]
fn rejects_wrong_publication_version_and_target() {
  let scratch = Scratch::new();
  let archive = package(&scratch.0, X64, files(X64));
  for (index, version, target) in [(0, "0.1.4", X64), (1, "0.1.3", ARM64)] {
    let staging = scratch.0.join(format!("stage-{index}"));
    fs::create_dir(&staging).unwrap();
    assert!(stage(&archive, &staging, version, target).is_err());
  }
}

#[test]
fn refuses_missing_helpers_and_modified_payloads() {
  let scratch = Scratch::new();
  let mut payload = files(X64);
  payload.remove("ctl-taskd.exe");
  let archive = package(&scratch.0, X64, payload);
  let staging = scratch.0.join("stage");
  fs::create_dir(&staging).unwrap();
  assert!(stage(&archive, &staging, "0.1.3", X64).is_err());
  fs::remove_file(archive).unwrap();
  fs::remove_dir_all(&staging).unwrap();
  fs::create_dir(&staging).unwrap();
  let archive = package(&scratch.0, X64, files(X64));
  let app = stage(&archive, &staging, "0.1.3", X64).unwrap();
  fs::write(app.join("ctld.exe"), b"modified").unwrap();
  assert!(verify(&app, "0.1.3", X64).is_err());
}

#[test]
fn rejects_windows_path_aliases_and_archive_links_before_extraction() {
  for unsafe_name in [
    "../outside",
    "ctmux/../outside",
    "ctmux\\escape",
    "ctmux/C:/escape",
    "ctmux/NUL.txt",
    "ctmux/trailing.",
    "ctmux/space ",
    "ctmux/a:stream",
  ] {
    let scratch = Scratch::new();
    let archive = scratch.0.join("bad.zip");
    let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
    zip
      .start_file(unsafe_name, SimpleFileOptions::default())
      .unwrap();
    zip.write_all(b"bad").unwrap();
    zip.finish().unwrap();
    assert!(
      stage(&archive, &scratch.0, "0.1.3", X64).is_err(),
      "{unsafe_name}"
    );
    assert!(!scratch.0.join("ctmux").exists());
  }
  let scratch = Scratch::new();
  let archive = scratch.0.join("link.zip");
  let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
  zip
    .add_symlink("ctmux/linked", "outside", SimpleFileOptions::default())
    .unwrap();
  zip.finish().unwrap();
  assert!(stage(&archive, &scratch.0, "0.1.3", X64).is_err());
}

#[test]
fn rejects_case_collisions_and_checksum_inventory_mismatches() {
  let scratch = Scratch::new();
  let mut buffer = Cursor::new(Vec::new());
  let mut zip = zip::ZipWriter::new(&mut buffer);
  for name in ["ctmux/ctld.exe", "ctmux/CTLD.EXE"] {
    zip.start_file(name, SimpleFileOptions::default()).unwrap();
    zip.write_all(b"duplicate").unwrap();
  }
  zip.finish().unwrap();
  let archive = scratch.0.join("collision.zip");
  fs::write(&archive, buffer.into_inner()).unwrap();
  assert!(stage(&archive, &scratch.0, "0.1.3", X64).is_err());
}

#[test]
fn equality_detects_extra_missing_and_changed_resources() {
  let scratch = Scratch::new();
  let archive = package(&scratch.0, X64, files(X64));
  let mut roots = Vec::new();
  for name in ["left", "right"] {
    let staging = scratch.0.join(name);
    fs::create_dir(&staging).unwrap();
    roots.push(stage(&archive, &staging, "0.1.3", X64).unwrap());
  }
  assert!(matches(&roots[0], &roots[1]).unwrap());
  fs::remove_file(roots[1].join("ctld.exe")).unwrap();
  assert!(!matches(&roots[0], &roots[1]).unwrap());
}

#[test]
fn rejects_duplicate_manifest_keys_and_path_resource_exhaustion() {
  let duplicate = br#"{"schema_version":1,"app_version":"0.1.3","target_triple":"x86_64-pc-windows-msvc","files":{"ctld.exe":"first","ctld.exe":"second"}}"#;
  assert!(serde_json::from_slice::<Manifest>(duplicate).is_err());
  for path in [
    "ctmux/é.txt".to_owned(),
    format!("ctmux/{}", "a".repeat(256)),
    format!("ctmux/{}", vec!["level"; 32].join("/")),
  ] {
    assert!(safe_path(&path).is_err(), "{path}");
  }
}

#[test]
fn rejects_dlls_and_truncated_section_tables_as_app_executables() {
  let scratch = Scratch::new();
  let image = scratch.0.join("image.exe");
  let mut dll = pe(0x8664);
  dll[86..88].copy_from_slice(&0x2002_u16.to_le_bytes());
  fs::write(&image, dll).unwrap();
  assert!(verify_pe(&image, 0x8664).is_err());
  let mut truncated = pe(0x8664);
  truncated.truncate(90);
  fs::write(&image, truncated).unwrap();
  assert!(verify_pe(&image, 0x8664).is_err());
}
