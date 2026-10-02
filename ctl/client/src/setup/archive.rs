use super::{Error, manifest::Manifest};
use flate2::read::MultiGzDecoder;
use sha2::{Digest as _, Sha256};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Component, Path};

const MAX_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;

struct Budget<R> {
  source: R,
  remaining: u64,
}

impl<R: Read> Read for Budget<R> {
  fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
    let limit = buffer
      .len()
      .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
    if limit == 0 && !buffer.is_empty() {
      return Err(io::Error::other(
        "ctld archive exceeds the extraction budget",
      ));
    }
    let count = self.source.read(&mut buffer[..limit])?;
    self.remaining -= count as u64;
    Ok(count)
  }
}

pub(super) fn extract(bytes: &[u8], manifest: &Manifest, destination: &Path) -> Result<(), Error> {
  verify_checksum(bytes, manifest)?;
  extract_bundle(bytes, manifest, destination)
}

fn verify_checksum(bytes: &[u8], manifest: &Manifest) -> Result<(), Error> {
  if bytes.len() as u64 != manifest.archive_size
    || format!("{:x}", Sha256::digest(bytes)) != manifest.sha256
  {
    return Err(Error::InvalidRelease(
      "archive length or SHA-256 checksum does not match".into(),
    ));
  }
  Ok(())
}

fn extract_bundle(bytes: &[u8], manifest: &Manifest, destination: &Path) -> Result<(), Error> {
  let reader = Budget {
    source: MultiGzDecoder::new(bytes),
    remaining: MAX_EXPANDED_BYTES,
  };
  let mut archive = tar::Archive::new(reader);
  let mut seen = HashSet::new();
  for entry in archive.entries()?.raw(true) {
    let mut entry = entry?;
    let path = entry.path()?.into_owned();
    let components: Vec<_> = path.components().collect();
    if components.first() != Some(&Component::Normal("ctld.app".as_ref()))
      || components
        .iter()
        .any(|component| !matches!(component, Component::Normal(_)))
      || path.to_str().is_none()
      || !seen.insert(path.clone())
      || seen.len() > MAX_ENTRIES
    {
      return Err(Error::InvalidRelease(
        "archive contains an unsafe, duplicate, or excessive path".into(),
      ));
    }
    let kind = entry.header().entry_type();
    if !kind.is_file() && !kind.is_dir() {
      return Err(Error::InvalidRelease(
        "archive contains links, extensions, or special files".into(),
      ));
    }
    let target = destination.join(&path);
    if kind.is_dir() {
      fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&target)?;
    } else {
      if entry.size() > MAX_EXPANDED_BYTES {
        return Err(Error::InvalidRelease(
          "archive file exceeds the extraction budget".into(),
        ));
      }
      fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(target.parent().unwrap())?;
      let mode = if path == Path::new("ctld.app/Contents/MacOS/ctld") {
        0o755
      } else {
        0o644
      };
      let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(target)?;
      if io::copy(&mut entry, &mut file)? != entry.size() {
        return Err(Error::InvalidRelease("truncated archive entry".into()));
      }
      file.flush()?;
      file.sync_all()?;
    }
  }
  // Consume padding and the gzip trailer as well, checking CRC and the same
  // decompression budget. Extra archives or payloads after the terminator fail.
  let mut reader = archive.into_inner();
  let mut buffer = [0_u8; 8192];
  loop {
    let count = reader.read(&mut buffer)?;
    if count == 0 {
      break;
    }
    if buffer[..count].iter().any(|byte| *byte != 0) {
      return Err(Error::InvalidRelease(
        "unexpected data after the archive terminator".into(),
      ));
    }
  }
  for name in [
    "Contents/Info.plist",
    "Contents/embedded.provisionprofile",
    "Contents/_CodeSignature/CodeResources",
    "Contents/MacOS/ctld",
  ] {
    if !destination.join("ctld.app").join(name).is_file() {
      return Err(Error::InvalidRelease(format!("archive is missing {name}")));
    }
  }
  if manifest.development.is_none()
    && !destination
      .join("ctld.app/Contents/CodeResources")
      .is_file()
  {
    return Err(Error::InvalidRelease(
      "archive is missing Contents/CodeResources".into(),
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn decompression_budget_counts_all_decoded_bytes() {
    let mut reader = Budget {
      source: &b"headers, padding, and payload"[..],
      remaining: 8,
    };
    let mut output = Vec::new();
    assert!(reader.read_to_end(&mut output).is_err());
    assert_eq!(output.len(), 8);
  }
}
