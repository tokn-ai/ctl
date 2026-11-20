use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
  pub handle: usize,
  pub process: u32,
  pub created: u64,
}

impl fmt::Display for Identity {
  fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      output,
      "win-{:x}-{:x}-{:x}",
      self.handle, self.process, self.created
    )
  }
}

impl FromStr for Identity {
  type Err = &'static str;

  fn from_str(value: &str) -> Result<Self, Self::Err> {
    let invalid = "invalid Windows window identifier";
    let mut parts = value.strip_prefix("win-").ok_or(invalid)?.split('-');
    let handle = usize::from_str_radix(parts.next().ok_or(invalid)?, 16).map_err(|_| invalid)?;
    let process = u32::from_str_radix(parts.next().ok_or(invalid)?, 16).map_err(|_| invalid)?;
    let created = u64::from_str_radix(parts.next().ok_or(invalid)?, 16).map_err(|_| invalid)?;
    let result = Self {
      handle,
      process,
      created,
    };
    if handle == 0
      || process == 0
      || created == 0
      || parts.next().is_some()
      || result.to_string() != value
    {
      return Err(invalid);
    }
    Ok(result)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn identifiers_bind_window_to_process_creation() {
    let identity = Identity {
      handle: 0x1a234,
      process: 0x57,
      created: 0x102030405060708,
    };
    let encoded = identity.to_string();
    assert_eq!(encoded, "win-1a234-57-102030405060708");
    assert_eq!(encoded.parse::<Identity>().unwrap(), identity);
    assert_ne!(
      encoded,
      Identity {
        created: identity.created + 1,
        ..identity
      }
      .to_string()
    );
  }

  #[test]
  fn malformed_or_noncanonical_identifiers_are_rejected() {
    for value in [
      "1",
      "win-1-2",
      "win-0-2-3",
      "win-1-0-3",
      "win-1-2-0",
      "win-1-2-3-4",
      "win-01-2-3",
      "win-A-2-3",
      "win-1-100000000-3",
      "win-1-2-10000000000000000",
      "win-+1-2-3",
    ] {
      assert!(value.parse::<Identity>().is_err(), "{value}");
    }
  }
}
