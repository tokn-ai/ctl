use super::{EXECUTABLES, MANIFEST, machine};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

pub(in crate::updater::app) const X64: &str = "x86_64-pc-windows-msvc";
pub(in crate::updater::app) const ARM64: &str = "aarch64-pc-windows-msvc";

pub(super) fn pe(machine: u16) -> Vec<u8> {
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

pub(in crate::updater::app) fn files(target: &str) -> BTreeMap<String, Vec<u8>> {
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

pub(in crate::updater::app) fn package(
  root: &Path,
  target: &str,
  mut files: BTreeMap<String, Vec<u8>>,
) -> PathBuf {
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
