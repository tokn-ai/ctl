//! Readable names and ordering for installed runtime builds.

use crate::component::ComponentBuildInfo;
use crate::installations::CatalogOrigin;
use semver::Version;
use std::cmp::Ordering;
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

fn channel(origin: Option<&CatalogOrigin>) -> &str {
  origin.map_or("untracked", |origin| origin.channel.as_str())
}

fn is_pull_request(channel: &str) -> bool {
  channel.strip_prefix("pr-").is_some_and(|number| {
    !number.starts_with('0')
      && number.bytes().all(|byte| byte.is_ascii_digit())
      && number.parse::<u64>().is_ok()
  })
}

fn version(version: &str) -> Option<Version> {
  let version = version.strip_prefix('v').unwrap_or(version);
  Version::parse(version).ok()
}

fn build_time(origin: Option<&CatalogOrigin>) -> Option<OffsetDateTime> {
  OffsetDateTime::parse(origin?.built_at.as_deref()?, &Rfc3339)
    .ok()
    .map(|date| date.to_offset(UtcOffset::UTC))
}

/// Returns a readable, portable directory component for a runtime build.
/// The calling store assigns a suffix if different builds share a label.
#[must_use]
pub fn label(
  build: &ComponentBuildInfo,
  origin: Option<&CatalogOrigin>,
  bundle_id: &str,
) -> String {
  label_from_version(
    &build.version,
    build.source_revision.as_deref(),
    origin,
    bundle_id,
  )
}

/// Formats a build label from package version and available provenance.
#[must_use]
pub fn label_from_version(
  app_version: &str,
  source_revision: Option<&str>,
  origin: Option<&CatalogOrigin>,
  content_id: &str,
) -> String {
  let version = version(app_version)
    .map(|version| version.to_string())
    .filter(|version| version.len() <= 72)
    .unwrap_or_else(|| "unknown".into());
  let revision = source_revision
    .and_then(|revision| revision.get(..7))
    .filter(|revision| revision.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .or_else(|| {
      content_id
        .get(..12)
        .filter(|id| id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    })
    .unwrap_or("unknown")
    .to_ascii_lowercase();
  let channel = channel(origin);
  if channel == "stable" {
    return format!("stable-v{version}-{revision}");
  }
  let date = build_time(origin).map_or_else(
    || "undated".into(),
    |date| {
      format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        date.year(),
        u8::from(date.month()),
        date.day(),
        date.hour(),
        date.minute(),
        date.second(),
      )
    },
  );
  if channel == "local" || is_pull_request(channel) {
    return format!("dev-{date}-v{version}-{revision}-{channel}");
  }
  let prefix = match channel {
    "nightly" | "release" | "provided" => channel,
    _ => "untracked",
  };
  format!("{prefix}-{date}-v{version}-{revision}")
}

fn group(channel: &str) -> u8 {
  match channel {
    "stable" => 0,
    "nightly" => 1,
    "local" => 2,
    channel if is_pull_request(channel) => 2,
    "release" => 3,
    "provided" => 4,
    _ => 5,
  }
}

/// Orders stable builds by descending `SemVer` precedence and other builds by
/// descending build time, with local and PR builds in the same group.
#[must_use]
pub fn compare(
  (left, left_origin): (&ComponentBuildInfo, Option<&CatalogOrigin>),
  (right, right_origin): (&ComponentBuildInfo, Option<&CatalogOrigin>),
) -> Ordering {
  let left_group = group(channel(left_origin));
  let right_group = group(channel(right_origin));
  left_group
    .cmp(&right_group)
    .then_with(|| {
      if left_group == 0 {
        match (version(&right.version), version(&left.version)) {
          (Some(right), Some(left)) => right.cmp_precedence(&left),
          (right, left) => right.is_some().cmp(&left.is_some()),
        }
      } else {
        Ordering::Equal
      }
    })
    .then_with(|| build_time(right_origin).cmp(&build_time(left_origin)))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn stable_uses_semver_and_development_uses_dates_across_sources() {
    let build = |version: &str| ComponentBuildInfo {
      version: version.into(),
      source_revision: Some("ba16876".to_owned() + &"0".repeat(33)),
      source_fingerprint: "a".repeat(64),
      dirty: false,
    };
    let origin = |channel: &str, date: &str| CatalogOrigin {
      channel: channel.into(),
      publication_id: None,
      built_at: Some(date.into()),
    };
    let stable = origin("stable", "2026-11-21T08:15:25Z");
    let local = origin("local", "2026-11-21T08:15:25Z");
    let pr = origin("pr-192", "2026-11-22T08:15:25Z");
    let id = &"a".repeat(64);
    assert_eq!(
      label(&build("0.1.4"), Some(&stable), id),
      "stable-v0.1.4-ba16876"
    );
    assert_eq!(
      label(&build("0.1.4"), Some(&local), id),
      "dev-20261121T081525Z-v0.1.4-ba16876-local",
    );
    assert_eq!(
      label(&build("0.1.4"), Some(&pr), id),
      "dev-20261122T081525Z-v0.1.4-ba16876-pr-192",
    );
    assert!(
      compare(
        (&build("0.1.10"), Some(&stable)),
        (&build("0.1.9"), Some(&stable)),
      )
      .is_lt()
    );
    assert!(
      compare(
        (&build("0.1.4"), Some(&pr)),
        (&build("0.1.5"), Some(&local)),
      )
      .is_lt()
    );
  }
}
