//! Compile-time channel policy shared with focused tests.

pub fn select(
  explicit: Option<&str>,
  packaged: bool,
  version: &str,
) -> Result<&'static str, &'static str> {
  match explicit {
    Some("dev") => Ok("dev"),
    Some("stable") => Ok("stable"),
    Some(_) => Err("CTL_BUNDLE_CHANNEL must be dev or stable"),
    None if packaged && !version.split('+').next().unwrap_or(version).contains('-') => Ok("stable"),
    None => Ok("dev"),
  }
}

#[cfg(test)]
mod tests {
  use super::select;

  #[test]
  fn published_packages_follow_stable_while_source_and_prerelease_builds_follow_dev() {
    assert_eq!(select(None, false, "0.1.1"), Ok("dev"));
    assert_eq!(select(None, true, "0.1.1"), Ok("stable"));
    assert_eq!(select(None, true, "0.1.1+build"), Ok("stable"));
    assert_eq!(select(None, true, "0.1.1-dev.1"), Ok("dev"));
    assert_eq!(select(Some("dev"), true, "0.1.1"), Ok("dev"));
    assert_eq!(select(Some("stable"), false, "0.1.1"), Ok("stable"));
    assert!(select(Some("latest"), false, "0.1.1").is_err());
  }
}
