//! Verified Windows portable packages. Kept platform-neutral for fixture tests.

use ctl_core::private_fs;
use serde::{Deserialize, Deserializer};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, Metadata};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 20_000;
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MANIFEST: &str = "portable.json";
const EXECUTABLES: [&str; 4] = ["ctmux-app.exe", "ctld.exe", "ctmuxd.exe", "ctl-taskd.exe"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
  pub schema_version: u16,
  pub app_version: String,
  pub target_triple: String,
  #[serde(deserialize_with = "unique_files")]
  pub files: BTreeMap<String, String>,
}

fn unique_files<'de, D: Deserializer<'de>>(
  deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error> {
  struct Unique;
  impl<'de> serde::de::Visitor<'de> for Unique {
    type Value = BTreeMap<String, String>;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      formatter.write_str("a portable file inventory with unique paths")
    }
    fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
      let mut files = BTreeMap::new();
      while let Some((name, digest)) = map.next_entry::<String, String>()? {
        if files.len() >= MAX_FILES || files.insert(name, digest).is_some() {
          return Err(serde::de::Error::custom(
            "duplicate or excessive portable file entries",
          ));
        }
      }
      Ok(files)
    }
  }
  deserializer.deserialize_map(Unique)
}

pub(super) fn stage(
  archive: &Path,
  staging: &Path,
  version: &str,
  target: &str,
) -> io::Result<PathBuf> {
  let mut zip = zip::ZipArchive::new(open_regular(archive)?).map_err(io::Error::other)?;
  if zip.len() > MAX_FILES {
    return Err(invalid("portable archive contains too many entries"));
  }
  let mut names = BTreeSet::new();
  let mut total = 0_u64;
  // Check every path before creating anything. Windows aliases are unsafe even
  // when this validator runs on a Unix build/test host.
  for index in 0..zip.len() {
    let entry = zip.by_index(index).map_err(io::Error::other)?;
    let name = entry.name().trim_end_matches('/');
    safe_path(name)?;
    if name != "ctmux" && !name.starts_with("ctmux/") {
      return Err(invalid(
        "portable archive must contain only the ctmux directory",
      ));
    }
    if !names.insert(name.to_ascii_lowercase()) {
      return Err(invalid("portable archive contains colliding paths"));
    }
    if let Some(mode) = entry.unix_mode() {
      let kind = mode & 0o170_000;
      if kind != 0 && kind != 0o100_000 && kind != 0o040_000 {
        return Err(invalid("portable archive contains a link or special file"));
      }
    }
    total = total
      .checked_add(entry.size())
      .ok_or_else(|| invalid("portable archive size overflow"))?;
    if total > MAX_BYTES {
      return Err(invalid("portable archive exceeds the expanded size limit"));
    }
  }
  let root = staging.join("ctmux");
  private_fs::create_directory(&root)?;
  for index in 0..zip.len() {
    let entry = zip.by_index(index).map_err(io::Error::other)?;
    let path = staging.join(entry.name().trim_end_matches('/'));
    if entry.is_dir() {
      ensure_directories(&root, &path)?;
    } else {
      ensure_directories(
        &root,
        path
          .parent()
          .ok_or_else(|| invalid("invalid portable archive path"))?,
      )?;
      let mut output = private_fs::options()
        .write(true)
        .create_new(true)
        .open(path)?;
      let size = entry.size();
      let copied = io::copy(&mut entry.take(size + 1), &mut output)?;
      if copied != size {
        return Err(invalid("portable archive entry size mismatch"));
      }
      output.sync_all()?;
    }
  }
  verify(&root, version, target)?;
  Ok(root)
}

pub(super) fn manifest(root: &Path) -> io::Result<Manifest> {
  let mut bytes = Vec::new();
  open_regular(&root.join(MANIFEST))?
    .take(4 * 1024 * 1024 + 1)
    .read_to_end(&mut bytes)?;
  if bytes.len() > 4 * 1024 * 1024 {
    return Err(invalid("portable manifest exceeds the size limit"));
  }
  let manifest: Manifest = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  if manifest.schema_version != 1 || manifest.files.len() > MAX_FILES {
    return Err(invalid("unsupported portable manifest"));
  }
  machine(&manifest.target_triple)?;
  if manifest.app_version.is_empty() {
    return Err(invalid("portable manifest has no app version"));
  }
  let mut names = BTreeSet::new();
  for (name, hash) in &manifest.files {
    safe_path(name)?;
    if name.eq_ignore_ascii_case(MANIFEST)
      || !names.insert(name.to_ascii_lowercase())
      || hash.len() != 64
      || !hash
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
      return Err(invalid("invalid portable manifest inventory"));
    }
  }
  if EXECUTABLES
    .iter()
    .any(|name| !manifest.files.contains_key(*name))
  {
    return Err(invalid("portable package is missing a required executable"));
  }
  Ok(manifest)
}

pub(super) fn verify(root: &Path, version: &str, target: &str) -> io::Result<()> {
  let manifest = manifest(root)?;
  if manifest.app_version != version || manifest.target_triple != target {
    return Err(invalid(
      "portable package version or target does not match its publication",
    ));
  }
  let mut actual = inventory(root)?;
  actual.remove(MANIFEST);
  if actual != manifest.files {
    return Err(invalid(
      "portable package file inventory or checksum mismatch",
    ));
  }
  for executable in EXECUTABLES {
    verify_pe(&root.join(executable), machine(target)?)?;
  }
  Ok(())
}

#[cfg(windows)]
pub(super) fn verify_existing(root: &Path, target: &str) -> io::Result<()> {
  let metadata = fs::symlink_metadata(root)?;
  if !private_fs::is_directory(&metadata) {
    return Err(invalid(
      "installed portable app must be a directory without reparse points",
    ));
  }
  if manifest(root)?.target_triple != target {
    return Err(invalid("installed portable app has a different target"));
  }
  // A changed/missing payload can be repaired, but never follow links while
  // inspecting or retaining the installed tree.
  inventory(root)?;
  Ok(())
}

pub(super) fn matches(left: &Path, right: &Path) -> io::Result<bool> {
  Ok(inventory(left)? == inventory(right)?)
}

fn inventory(root: &Path) -> io::Result<BTreeMap<String, String>> {
  fn visit(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, String>,
    bytes: &mut u64,
    entries: &mut usize,
  ) -> io::Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    if !private_fs::is_directory(&metadata) {
      return Err(invalid(
        "portable app must contain directories without reparse points",
      ));
    }
    for entry in fs::read_dir(directory)? {
      let entry = entry?;
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      *entries += 1;
      if *entries > MAX_FILES {
        return Err(invalid("portable app exceeds inventory entry limits"));
      }
      let name = path
        .strip_prefix(root)
        .map_err(io::Error::other)?
        .components()
        .map(|part| {
          part
            .as_os_str()
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 portable path"))
        })
        .collect::<io::Result<Vec<_>>>()?
        .join("/");
      safe_path(&format!("ctmux/{name}"))?;
      if metadata.is_dir() {
        visit(root, &path, files, bytes, entries)?;
      } else {
        require_regular(&metadata)?;
        *bytes = bytes
          .checked_add(metadata.len())
          .ok_or_else(|| invalid("portable app size overflow"))?;
        if files.len() >= MAX_FILES || *bytes > MAX_BYTES {
          return Err(invalid("portable app exceeds inventory limits"));
        }
        let mut hash = Sha256::new();
        io::copy(&mut open_regular(&path)?, &mut hash)?;
        files.insert(name, format!("{:x}", hash.finalize()));
      }
    }
    Ok(())
  }
  let mut files = BTreeMap::new();
  visit(root, root, &mut files, &mut 0, &mut 1)?;
  Ok(files)
}

fn verify_pe(path: &Path, expected: u16) -> io::Result<()> {
  let mut file = open_regular(path)?;
  let mut dos = [0_u8; 64];
  file.read_exact(&mut dos)?;
  let offset = u32::from_le_bytes(dos[60..64].try_into().map_err(io::Error::other)?);
  if &dos[..2] != b"MZ" || !(64..=16 * 1024 * 1024).contains(&offset) {
    return Err(invalid("portable executable has an invalid DOS header"));
  }
  file.seek(SeekFrom::Start(u64::from(offset)))?;
  let mut pe = [0_u8; 26];
  file.read_exact(&mut pe)?;
  let sections = u16::from_le_bytes([pe[6], pe[7]]);
  let optional_size = u16::from_le_bytes([pe[20], pe[21]]);
  let characteristics = u16::from_le_bytes([pe[22], pe[23]]);
  if &pe[..4] != b"PE\0\0"
    || u16::from_le_bytes([pe[4], pe[5]]) != expected
    || !(1..=96).contains(&sections)
    || optional_size < 112
    || u64::from(offset) + 24 + u64::from(optional_size) + u64::from(sections) * 40
      > file.metadata()?.len()
    || characteristics & 2 == 0
    || characteristics & 0x2000 != 0
    || u16::from_le_bytes([pe[24], pe[25]]) != 0x20b
  {
    return Err(invalid(
      "portable executable is not a matching Windows PE32+ image",
    ));
  }
  Ok(())
}

fn machine(target: &str) -> io::Result<u16> {
  match target {
    "x86_64-pc-windows-msvc" => Ok(0x8664),
    "aarch64-pc-windows-msvc" => Ok(0xaa64),
    _ => Err(invalid("unsupported Windows portable target")),
  }
}

fn safe_path(path: &str) -> io::Result<()> {
  if path.is_empty() || path.len() > 1024 || !path.is_ascii() || path.split('/').count() > 32 {
    return Err(invalid("invalid portable path length"));
  }
  for part in path.split('/') {
    let base = part
      .split('.')
      .next()
      .unwrap_or_default()
      .to_ascii_uppercase();
    if part.is_empty()
      || part.len() > 255
      || part == "."
      || part == ".."
      || part.ends_with(['.', ' '])
      || part
        .chars()
        .any(|ch| ch.is_control() || "\\:<>\"|?*".contains(ch))
      || matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
      )
      || (base.len() == 4
        && (base.starts_with("COM") || base.starts_with("LPT"))
        && matches!(base.as_bytes()[3], b'1'..=b'9'))
    {
      return Err(invalid(&format!(
        "unsafe or reserved Windows portable path: {path:?}"
      )));
    }
  }
  Ok(())
}

fn ensure_directories(root: &Path, path: &Path) -> io::Result<()> {
  let mut directory = root.to_owned();
  for part in std::iter::once(None).chain(
    path
      .strip_prefix(root)
      .map_err(io::Error::other)?
      .components()
      .map(Some),
  ) {
    if let Some(part) = part {
      directory.push(part);
      match private_fs::create_directory(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
      }
    }
    if !private_fs::is_directory(&fs::symlink_metadata(&directory)?) {
      return Err(invalid(
        "portable app must contain owned directories without links",
      ));
    }
  }
  Ok(())
}

fn open_regular(path: &Path) -> io::Result<File> {
  let file = private_fs::options().read(true).open(path)?;
  require_regular(&file.metadata()?)?;
  Ok(file)
}

fn require_regular(metadata: &Metadata) -> io::Result<()> {
  if private_fs::is_owned_file(metadata) {
    Ok(())
  } else {
    Err(invalid("portable payload must contain regular files"))
  }
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
pub(super) mod fixtures;

#[cfg(test)]
mod tests;
