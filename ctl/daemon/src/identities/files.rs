use base64::Engine as _;
use ctld_ipc::identities::{FileState, IdentityFile, PassphraseState};
use sha2::{Digest, Sha256};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

use super::IdentityError;

mod pem;

pub const MAX_KEY_BYTES: usize = 1024 * 1024;

/// A bounded snapshot. Secret key bytes are deliberately private and not Debug.
pub struct IdentitySnapshot {
  pub identity_id: String,
  pub path: String,
  pub file_version: String,
  pub key_type: Option<String>,
  pub fingerprint: Option<String>,
  pub public_key: Option<String>,
  pub encrypted: bool,
  pub(super) bytes: Zeroizing<Vec<u8>>,
}

impl IdentitySnapshot {
  pub(super) fn record(&self, available: bool) -> IdentityFile {
    IdentityFile {
      identity_id: self.identity_id.clone(),
      path: self.path.clone(),
      display_path: display_path(&self.path),
      file_version: Some(self.file_version.clone()),
      // Envelope metadata is useful internally for matching, but is not proof
      // that the encrypted private key matches it. Inventory exposes only the
      // verified metadata projected from a matching saved binding.
      key_type: None,
      fingerprint: None,
      encrypted: Some(self.encrypted),
      file_state: FileState::Ready,
      passphrase_state: if !self.encrypted {
        PassphraseState::NotRequired
      } else if available {
        PassphraseState::NotSaved
      } else {
        PassphraseState::Unknown
      },
      detail: None,
    }
  }
}

/// Read a regular identity file without following a changed file after opening.
///
/// # Errors
/// Returns a sanitized error for missing, unreadable, oversized, or unsupported files.
pub fn inspect_path(path: &str) -> Result<IdentitySnapshot, IdentityError> {
  let path = expand_path(path)?;
  let canonical = std::fs::canonicalize(path).map_err(IdentityError::file_io)?;
  let path = canonical
    .to_str()
    .ok_or(IdentityError::InvalidRequest)?
    .to_owned();
  if !std::fs::metadata(&canonical)
    .map_err(IdentityError::file_io)?
    .is_file()
  {
    return Err(IdentityError::UnsupportedFile);
  }
  let mut options = std::fs::OpenOptions::new();
  options.read(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.custom_flags(
      i32::try_from(rustix::fs::OFlags::NONBLOCK.bits())
        .map_err(|_| IdentityError::UnreadableFile)?,
    );
  }
  let file = options.open(canonical).map_err(IdentityError::file_io)?;
  let metadata = file.metadata().map_err(IdentityError::file_io)?;
  if !metadata.is_file() || metadata.len() > MAX_KEY_BYTES as u64 {
    return Err(IdentityError::UnsupportedFile);
  }
  let mut bytes = Zeroizing::new(Vec::new());
  file
    .take((MAX_KEY_BYTES + 1) as u64)
    .read_to_end(&mut bytes)
    .map_err(IdentityError::file_io)?;
  if bytes.len() > MAX_KEY_BYTES {
    return Err(IdentityError::UnsupportedFile);
  }
  let envelope = envelope(&bytes)?;
  Ok(IdentitySnapshot {
    identity_id: digest(path.as_bytes()),
    path,
    file_version: digest(&bytes),
    key_type: envelope.key_type,
    fingerprint: envelope.fingerprint,
    public_key: envelope.public_key,
    encrypted: envelope.encrypted,
    bytes,
  })
}

pub(super) fn expand_path(path: &str) -> Result<PathBuf, IdentityError> {
  if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
    return Err(IdentityError::InvalidRequest);
  }
  let expanded = if let Some(relative) = path.strip_prefix("~/") {
    dirs::home_dir()
      .ok_or(IdentityError::UnreadableFile)?
      .join(relative)
  } else {
    PathBuf::from(path)
  };
  if !expanded.is_absolute() {
    return Err(IdentityError::InvalidRequest);
  }
  Ok(expanded)
}

pub(super) fn display_path(path: &str) -> String {
  dirs::home_dir()
    .and_then(|home| {
      Path::new(path)
        .strip_prefix(home)
        .ok()
        .map(Path::to_path_buf)
    })
    .map_or_else(
      || path.to_owned(),
      |relative| format!("~/{}", relative.display()),
    )
}

pub(super) fn digest(value: &[u8]) -> String {
  use std::fmt::Write as _;
  Sha256::digest(value)
    .iter()
    .fold(String::with_capacity(64), |mut out, byte| {
      write!(out, "{byte:02x}").expect("writing to String cannot fail");
      out
    })
}

pub(super) fn valid_id(value: &str) -> bool {
  value.len() == 64
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

struct Envelope {
  encrypted: bool,
  key_type: Option<String>,
  fingerprint: Option<String>,
  public_key: Option<String>,
}

fn envelope(bytes: &[u8]) -> Result<Envelope, IdentityError> {
  let text = std::str::from_utf8(bytes).map_err(|_| IdentityError::UnsupportedFile)?;
  if let Some(encoded) = text.strip_prefix("-----BEGIN OPENSSH PRIVATE KEY-----") {
    let (encoded, remainder) = encoded
      .split_once("-----END OPENSSH PRIVATE KEY-----")
      .ok_or(IdentityError::UnsupportedFile)?;
    if !remainder.trim().is_empty() {
      return Err(IdentityError::UnsupportedFile);
    }
    let encoded = Zeroizing::new(
      encoded
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>(),
    );
    let decoded = Zeroizing::new(
      base64::engine::general_purpose::STANDARD
        .decode(&*encoded)
        .map_err(|_| IdentityError::UnsupportedFile)?,
    );
    let mut remaining = decoded
      .strip_prefix(b"openssh-key-v1\0")
      .ok_or(IdentityError::UnsupportedFile)?;
    let cipher = take_string(&mut remaining)?;
    let _kdf = take_string(&mut remaining)?;
    let _options = take_string(&mut remaining)?;
    if take_u32(&mut remaining)? != 1 {
      return Err(IdentityError::UnsupportedFile);
    }
    let public_key = take_string(&mut remaining)?;
    let (key_type, fingerprint) = public_metadata(public_key)?;
    let private = take_string(&mut remaining)?;
    if private.is_empty() || !remaining.is_empty() {
      return Err(IdentityError::UnsupportedFile);
    }
    return Ok(Envelope {
      encrypted: cipher != b"none",
      public_key: Some(format!(
        "{key_type} {}",
        base64::engine::general_purpose::STANDARD.encode(public_key)
      )),
      key_type: Some(key_type),
      fingerprint: Some(fingerprint),
    });
  }
  for (header, key_type) in [
    ("RSA PRIVATE KEY", Some("ssh-rsa")),
    ("EC PRIVATE KEY", None),
    ("DSA PRIVATE KEY", Some("ssh-dss")),
    ("PRIVATE KEY", None),
    ("ENCRYPTED PRIVATE KEY", None),
  ] {
    if text.starts_with(&format!("-----BEGIN {header}-----"))
      && text.contains(&format!("-----END {header}-----"))
    {
      return Ok(Envelope {
        encrypted: pem::encrypted(text, header)?,
        key_type: key_type.map(str::to_owned),
        fingerprint: None,
        public_key: None,
      });
    }
  }
  Err(IdentityError::UnsupportedFile)
}

pub(super) fn public_metadata(blob: &[u8]) -> Result<(String, String), IdentityError> {
  let mut remaining = blob;
  let key_type = std::str::from_utf8(take_string(&mut remaining)?)
    .map_err(|_| IdentityError::UnsupportedFile)?;
  if key_type.is_empty()
    || key_type.len() > 128
    || !key_type
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b"-@._".contains(&b))
  {
    return Err(IdentityError::UnsupportedFile);
  }
  let fingerprint = format!(
    "SHA256:{}",
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(blob))
  );
  Ok((key_type.to_owned(), fingerprint))
}

fn take_u32(input: &mut &[u8]) -> Result<usize, IdentityError> {
  let (prefix, tail) = input
    .split_at_checked(4)
    .ok_or(IdentityError::UnsupportedFile)?;
  *input = tail;
  Ok(u32::from_be_bytes(
    prefix
      .try_into()
      .map_err(|_| IdentityError::UnsupportedFile)?,
  ) as usize)
}

fn take_string<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], IdentityError> {
  let size = take_u32(input)?;
  let (value, tail) = input
    .split_at_checked(size)
    .ok_or(IdentityError::UnsupportedFile)?;
  *input = tail;
  Ok(value)
}
