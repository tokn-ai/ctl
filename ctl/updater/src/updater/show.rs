//! Installed builds are separate from the stable logical component names.

use super::{TargetOptions, remote::Target, status};
use ctl_client::installation::{InstallationShow, InstalledBuild};
use ctl_core::bundles::Source;
use ctl_core::installations::CatalogEntry;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io;
use std::path::Path;

/// Reads a passive inventory of catalogued installations.
pub(super) async fn snapshot(
  home: &Path,
  target: TargetOptions<'_>,
) -> io::Result<InstallationShow> {
  if target.remote() {
    crate::bundled::register()?;
    let target = status::remote_target(target).await?;
    remote_snapshot(&target).await
  } else {
    ctl_client::installation::show(home)
  }
}

async fn remote_snapshot(target: &Target) -> io::Result<InstallationShow> {
  let expected = target.expected_account();
  if let Some(snapshot) = ctl_client::installation::remote_show(
    &target.destination,
    &target.options,
    &target.interaction,
    expected,
  )
  .await
  .map_err(io::Error::other)?
  {
    return Ok(snapshot);
  }
  let status = ctl_client::installation::remote_status(
    &target.destination,
    &target.options,
    &target.interaction,
    expected,
  )
  .await
  .map_err(io::Error::other)?;
  let catalog = ctl_client::installation::remote_catalog(
    &target.destination,
    &target.options,
    &target.interaction,
    expected,
  )
  .await
  .map_err(io::Error::other)?;
  InstallationShow::from_status(status, catalog)
}

pub(super) fn print(snapshot: &InstallationShow, json: bool, development: bool) -> io::Result<()> {
  if json {
    println!(
      "{}",
      serde_json::to_string(snapshot).map_err(io::Error::other)?
    );
  } else {
    println!("{}", render(snapshot, development));
  }
  Ok(())
}

pub(super) fn render(snapshot: &InstallationShow, development: bool) -> String {
  let rows = build_rows(snapshot);
  if rows.is_empty() {
    let next = if development {
      "Run `ctlup dev` to build and install this checkout."
    } else {
      "Run `ctlup install --help` for installation options."
    };
    return format!("No managed ctl installation.\n{next}");
  }
  let ids = rows.keys().map(String::as_str).collect::<Vec<_>>();
  let selection = |id: Option<&str>| {
    let Some(id) = id else {
      return "none".into();
    };
    let short = short_id(id, &ids);
    if rows.get(id).is_some_and(|row| row.missing) {
      format!("{short} (missing)")
    } else {
      short
    }
  };
  let mut ordered = rows.values().collect::<Vec<_>>();
  ordered.sort_by_key(|row| (row.rank(snapshot), row.target(), row.id));
  let table = crate::table::format(
    [
      "BUILD",
      "VERSION",
      "REVISION",
      "SOURCE",
      "CHANNELS",
      "COMPONENT",
      "BUILD DATE",
      "STATE",
    ],
    ordered.into_iter().map(|row| row.cells(snapshot, &ids)),
  );
  let mut text = format!("Active: {}\n", selection(snapshot.active.as_deref()));
  if let Some(origin) = &snapshot.active_origin {
    let source = if super::tracked_channel(Some(origin)).is_some() {
      "Tracking"
    } else {
      "Pinned to"
    };
    let _ = write!(text, "{source} {}", origin.channel);
    if let Some(id) = &origin.publication_id {
      let _ = write!(text, " ({id})");
    }
    text.push('\n');
  }
  if let Some(previous) = &snapshot.previous {
    let _ = writeln!(text, "Previous: {}", selection(Some(previous)));
  }
  let _ = write!(text, "\nInstalled builds\n{table}");
  if !snapshot.missing_builds.is_empty() {
    text.push_str(
      "\n\nOn the machine with the missing builds, use `ctlup update` for a release or `ctlup dev` for a checkout build. Reinstall supplied builds from their original source.",
    );
  }
  text
}

struct BuildRow<'a> {
  id: &'a str,
  build: Option<&'a InstalledBuild>,
  entries: Vec<&'a CatalogEntry>,
  missing: bool,
}

impl BuildRow<'_> {
  fn target(&self) -> &str {
    self
      .build
      .map(|build| build.target_triple.as_str())
      .or_else(|| {
        self
          .entries
          .first()
          .map(|entry| entry.target_triple.as_str())
      })
      .unwrap_or("unknown")
  }

  fn states(&self, snapshot: &InstallationShow) -> Vec<&'static str> {
    let mut states = Vec::new();
    if snapshot.active.as_deref() == Some(self.id) {
      states.push("active");
    }
    if snapshot.previous.as_deref() == Some(self.id) {
      states.push("previous");
    }
    if self.entries.iter().any(|entry| entry.current) {
      states.push("current");
    }
    if states.is_empty() {
      states.push("retained");
    }
    if self.missing {
      states.push("missing");
    }
    states
  }

  fn rank(&self, snapshot: &InstallationShow) -> u8 {
    if snapshot.active.as_deref() == Some(self.id) {
      0
    } else if snapshot.previous.as_deref() == Some(self.id) {
      1
    } else if self.entries.iter().any(|entry| entry.current) {
      2
    } else {
      3
    }
  }

  fn channels(&self) -> String {
    let mut channels = BTreeMap::<&str, bool>::new();
    for entry in &self.entries {
      if let Some(origin) = &entry.origin {
        *channels.entry(&origin.channel).or_default() |= entry.current;
      }
    }
    if channels.is_empty() {
      "unknown".into()
    } else {
      channels
        .into_iter()
        .map(|(channel, current)| {
          let state = if current { "current" } else { "retained" };
          format!("{channel} ({state})")
        })
        .collect::<Vec<_>>()
        .join(", ")
    }
  }

  fn build_date(&self) -> String {
    let dates = self
      .entries
      .iter()
      .filter_map(|entry| entry.origin.as_ref()?.built_at.as_deref())
      .collect::<BTreeSet<_>>();
    if dates.is_empty() {
      "unknown".into()
    } else {
      dates.into_iter().collect::<Vec<_>>().join(", ")
    }
  }

  fn cells(&self, snapshot: &InstallationShow, ids: &[&str]) -> [String; 8] {
    let (version, revision, source) = self.build.map_or_else(
      || ("unknown".into(), "unknown".into(), "unknown"),
      |summary| {
        let dirty = if summary.build.dirty { " (dirty)" } else { "" };
        let revision = summary.build.source_revision.as_ref().map_or_else(
          || "unknown".into(),
          |revision| revision.chars().take(12).collect(),
        );
        let source = match summary.source {
          Source::Ci => "ci",
          Source::Release => "release",
          Source::Local => "local",
        };
        (
          format!("{}{dirty}", summary.build.version),
          revision,
          source,
        )
      },
    );
    let component = if self.target() == "unknown" {
      "unknown".into()
    } else {
      format!("bundle-{}", self.target())
    };
    [
      short_id(self.id, ids),
      version,
      revision,
      source.into(),
      self.channels(),
      component,
      self.build_date(),
      self.states(snapshot).join(", "),
    ]
  }
}

fn build_rows(snapshot: &InstallationShow) -> BTreeMap<String, BuildRow<'_>> {
  let mut rows = BTreeMap::new();
  let missing: BTreeSet<_> = snapshot.missing_builds.iter().map(String::as_str).collect();
  for build in &snapshot.builds {
    rows.insert(
      build.bundle_id.clone(),
      BuildRow {
        id: &build.bundle_id,
        build: Some(build),
        entries: Vec::new(),
        missing: false,
      },
    );
  }
  for entry in &snapshot.installations {
    let id = entry.bundle_id.as_str();
    rows
      .entry(entry.bundle_id.clone())
      .or_insert_with(|| BuildRow {
        id,
        build: None,
        entries: Vec::new(),
        missing: missing.contains(id),
      })
      .entries
      .push(entry);
  }
  rows
}

fn short_id(id: &str, ids: &[&str]) -> String {
  let mut length = 12.min(id.len());
  while length < id.len()
    && ids
      .iter()
      .any(|other| *other != id && other.as_bytes().get(..length) == id.as_bytes().get(..length))
  {
    length += 1;
  }
  id.chars().take(length).collect()
}

#[cfg(test)]
mod tests;
