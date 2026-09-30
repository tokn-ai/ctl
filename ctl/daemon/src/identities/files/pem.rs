//! Bounded PEM/DER shape checks, without claiming cryptographic verification.

use base64::Engine as _;
use zeroize::Zeroizing;

use super::IdentityError;

pub(super) fn encrypted(text: &str, header: &str) -> Result<bool, IdentityError> {
  let body = text
    .strip_prefix(&format!("-----BEGIN {header}-----"))
    .ok_or(IdentityError::UnsupportedFile)?;
  let (body, remainder) = body
    .split_once(&format!("-----END {header}-----"))
    .ok_or(IdentityError::UnsupportedFile)?;
  if !remainder.trim().is_empty() {
    return Err(IdentityError::UnsupportedFile);
  }
  let mut encoded = Zeroizing::new(String::new());
  let mut proc_type = false;
  let mut cipher_header = false;
  for line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
    if line == "Proc-Type: 4,ENCRYPTED" && encoded.is_empty() && !proc_type {
      proc_type = true;
    } else if let Some(cipher) = line.strip_prefix("DEK-Info: ") {
      let (cipher, iv) = cipher
        .split_once(',')
        .ok_or(IdentityError::UnsupportedFile)?;
      if !encoded.is_empty()
        || cipher_header
        || cipher.is_empty()
        || iv.len() < 16
        || !iv.bytes().all(|b| b.is_ascii_hexdigit())
      {
        return Err(IdentityError::UnsupportedFile);
      }
      cipher_header = true;
    } else {
      encoded.push_str(line);
    }
  }
  let decoded = Zeroizing::new(
    base64::engine::general_purpose::STANDARD
      .decode(encoded.as_bytes())
      .map_err(|_| IdentityError::UnsupportedFile)?,
  );
  if proc_type || cipher_header {
    if !proc_type
      || !cipher_header
      || decoded.len() < 16
      || matches!(header, "PRIVATE KEY" | "ENCRYPTED PRIVATE KEY")
    {
      return Err(IdentityError::UnsupportedFile);
    }
    return Ok(true);
  }
  validate_der(&decoded, header).ok_or(IdentityError::UnsupportedFile)?;
  Ok(header == "ENCRYPTED PRIVATE KEY")
}

fn validate_der(bytes: &[u8], header: &str) -> Option<()> {
  let mut bytes = bytes;
  let mut fields = der(&mut bytes, 0x30)?;
  if !bytes.is_empty() {
    return None;
  }
  match header {
    "ENCRYPTED PRIVATE KEY" => {
      algorithm(der(&mut fields, 0x30)?)?;
      nonempty(der(&mut fields, 0x04)?)?;
    }
    "RSA PRIVATE KEY" => {
      let version = der(&mut fields, 0x02)?;
      if !matches!(version, [0 | 1]) {
        return None;
      }
      for _ in 0..8 {
        nonempty(der(&mut fields, 0x02)?)?;
      }
      if version == [1] {
        nonempty(der(&mut fields, 0x30)?)?;
      }
    }
    "DSA PRIVATE KEY" => {
      if der(&mut fields, 0x02)? != [0] {
        return None;
      }
      for _ in 0..5 {
        nonempty(der(&mut fields, 0x02)?)?;
      }
    }
    "EC PRIVATE KEY" | "PRIVATE KEY" => {
      let version = der(&mut fields, 0x02)?;
      if (header == "EC PRIVATE KEY" && version != [1])
        || (header == "PRIVATE KEY" && !matches!(version, [0 | 1]))
      {
        return None;
      }
      if header == "PRIVATE KEY" {
        algorithm(der(&mut fields, 0x30)?)?;
      }
      nonempty(der(&mut fields, 0x04)?)?;
      while let Some(tag @ (0xa0 | 0xa1 | 0x81)) = fields.first().copied() {
        nonempty(der(&mut fields, tag)?)?;
      }
    }
    _ => return None,
  }
  fields.is_empty().then_some(())
}

fn nonempty(bytes: &[u8]) -> Option<()> {
  (!bytes.is_empty()).then_some(())
}

fn algorithm(mut fields: &[u8]) -> Option<()> {
  nonempty(der(&mut fields, 0x06)?)?;
  while let Some(tag) = fields.first().copied() {
    der(&mut fields, tag)?;
  }
  Some(())
}

fn der<'a>(bytes: &mut &'a [u8], tag: u8) -> Option<&'a [u8]> {
  if bytes.first().copied()? != tag {
    return None;
  }
  let length = *bytes.get(1)?;
  *bytes = &bytes[2..];
  let length = if length < 128 {
    usize::from(length)
  } else {
    let count = usize::from(length & 0x7f);
    if count == 0 || count > 4 {
      return None;
    }
    let (length, remaining) = bytes.split_at_checked(count)?;
    *bytes = remaining;
    if length.first() == Some(&0) {
      return None;
    }
    let size = length
      .iter()
      .fold(0_usize, |value, byte| (value << 8) | usize::from(*byte));
    if size < 128 {
      return None;
    }
    size
  };
  let (value, remaining) = bytes.split_at_checked(length)?;
  *bytes = remaining;
  Some(value)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn malformed_pem_headers_body_and_trailing_data_are_rejected() {
    for text in [
      "-----BEGIN PRIVATE KEY-----\nZmFrZQ==\n-----END PRIVATE KEY-----",
      "-----BEGIN PRIVATE KEY-----\nnot base64!\n-----END PRIVATE KEY-----",
      "-----BEGIN PRIVATE KEY-----\nMAACAQA=\n-----END PRIVATE KEY----- trailing",
      "-----BEGIN PRIVATE KEY-----\nMAACAQA=",
    ] {
      assert!(encrypted(text, "PRIVATE KEY").is_err());
    }
  }

  #[test]
  fn indefinite_and_truncated_der_lengths_are_rejected() {
    assert!(validate_der(&[0x30, 0x80, 0, 0], "PRIVATE KEY").is_none());
    assert!(validate_der(&[0x30, 0x82, 0xff, 0xff, 0], "PRIVATE KEY").is_none());
  }
}
