//! Bounded unpacking shared by remote installers, without filesystem writes.
use ctl_core::bundle_layout::{Layout, MAX_FILE_BYTES, MAX_MANIFEST_BYTES, MAX_TOTAL_BYTES};
use std::collections::BTreeMap;
use std::io::{self, Read as _};

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) fn public_layout(files: &mut BTreeMap<String, Vec<u8>>) -> io::Result<Layout> {
  #[derive(serde::Deserialize)]
  struct Header {
    schema_version: u32,
  }

  // Published schema 1 (including historical archives without a manifest)
  // and schema 2 have four services. Only schema 3 includes both clients.
  let layout = match files.remove("manifest.json") {
    None => Layout::Services,
    Some(bytes) if bytes.len() <= super::MAX_BUNDLE_SET_BYTES => {
      let header: Header = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
      Layout::public(header.schema_version)
        .ok_or_else(|| invalid("unknown public bundle schema"))?
    }
    Some(_) => return Err(invalid("public bundle manifest exceeds its size limit")),
  };
  if !layout.matches(files) {
    return Err(invalid(
      "public bundle executables disagree with the declared schema",
    ));
  }
  Ok(layout)
}

pub(crate) fn read_archive(bytes: &[u8]) -> io::Result<BTreeMap<String, Vec<u8>>> {
  if bytes.len() > MAX_FILE_BYTES {
    return Err(invalid("component archive exceeds its size limit"));
  }
  let decoder = flate2::read::MultiGzDecoder::new(bytes).take(super::MAX_TAR_BYTES + 1);
  let mut archive = tar::Archive::new(decoder);
  let mut files = BTreeMap::new();
  let mut total = 0usize;
  for entry in archive.entries()?.raw(true) {
    let mut entry = entry?;
    let name = std::str::from_utf8(&entry.path_bytes())
      .map_err(io::Error::other)?
      .to_owned();
    if entry.header().entry_type() != tar::EntryType::Regular
      || entry.size() == 0
      || entry.size() > MAX_FILE_BYTES as u64
      || files.len() > 512
    {
      return Err(invalid("unsafe or excessive component archive entries"));
    }
    let mut payload = Vec::new();
    entry.read_to_end(&mut payload)?;
    total = total.saturating_add(payload.len());
    if total > MAX_TOTAL_BYTES + MAX_MANIFEST_BYTES {
      return Err(invalid("component archive exceeds its payload limit"));
    }
    if name.contains(['\\', '\0'])
      || name
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
      return Err(invalid("unsafe component archive path"));
    }
    if files.insert(name, payload).is_some() {
      return Err(invalid("duplicate component archive entry"));
    }
  }
  let mut padding = archive.into_inner();
  let mut buffer = [0; 8192];
  loop {
    let read = padding.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    if buffer[..read].iter().any(|byte| *byte != 0) {
      return Err(invalid("component archive contains trailing data"));
    }
  }
  if padding.limit() == 0 {
    return Err(invalid("component archive exceeds its expanded limit"));
  }
  Ok(files)
}
