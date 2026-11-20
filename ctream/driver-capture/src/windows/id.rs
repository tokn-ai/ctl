pub(super) fn display_number(name: &str) -> Option<u32> {
  let digits = name.strip_prefix(r"\\.\DISPLAY")?;
  if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
    return None;
  }
  digits.parse().ok().filter(|number| *number != 0)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn display_numbers_come_from_device_names_not_adapter_order() {
    assert_eq!(display_number(r"\\.\DISPLAY12"), Some(12));
    assert_eq!(display_number(r"\\.\DISPLAY1"), Some(1));
    for name in [
      "DISPLAY1",
      r"\\.\DISPLAY",
      r"\\.\DISPLAY0",
      r"\\.\DISPLAY-1",
      r"\\.\DISPLAY+1",
      r"\\.\DISPLAY1suffix",
      r"\\.\DISPLAY4294967296",
    ] {
      assert_eq!(display_number(name), None, "{name}");
    }
  }
}
