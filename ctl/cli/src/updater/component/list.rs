//! Logical component names remain stable when installed builds change.

use ctl_client::remote_bundle::published::PublishedTarget;
use ctl_core::installations::CatalogEntry;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Serialize)]
pub(super) struct ComponentList {
  schema_version: u16,
  components: Vec<Component>,
}

#[derive(Debug, Serialize)]
struct Component {
  name: String,
  target_triple: String,
  installed: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  compatible: Option<bool>,
}

pub(super) fn installed(entries: &[CatalogEntry], channel: Option<&str>) -> ComponentList {
  let targets = entries
    .iter()
    .filter(|entry| {
      channel.is_none_or(|channel| {
        entry
          .origin
          .as_ref()
          .is_some_and(|origin| origin.channel == channel)
      })
    })
    .map(|entry| entry.target_triple.as_str())
    .collect::<BTreeSet<_>>();
  ComponentList {
    schema_version: 1,
    components: targets
      .into_iter()
      .map(|target| Component::new(target.into(), true, None))
      .collect(),
  }
}

pub(super) fn available(
  targets: Vec<PublishedTarget>,
  entries: &[CatalogEntry],
  channel: &str,
) -> ComponentList {
  let installed = entries
    .iter()
    .filter(|entry| {
      entry.current
        && entry
          .origin
          .as_ref()
          .is_some_and(|origin| origin.channel == channel)
    })
    .map(|entry| entry.target_triple.as_str())
    .collect::<BTreeSet<_>>();
  let mut components = installed
    .iter()
    .map(|target| ((*target).to_owned(), None))
    .collect::<BTreeMap<_, _>>();
  components.extend(
    targets
      .into_iter()
      .map(|target| (target.target_triple, Some(target.compatible))),
  );
  ComponentList {
    schema_version: 1,
    components: components
      .into_iter()
      .map(|(target, compatible)| {
        let present = installed.contains(target.as_str());
        Component::new(target, present, compatible)
      })
      .collect(),
  }
}

impl Component {
  fn new(target_triple: String, installed: bool, compatible: Option<bool>) -> Self {
    Self {
      name: format!("bundle-{target_triple}"),
      target_triple,
      installed,
      compatible,
    }
  }
}

impl ComponentList {
  pub(super) fn render(&self) -> String {
    if self.components.is_empty() {
      return "No components.".into();
    }
    self
      .components
      .iter()
      .map(|component| {
        let mut markers = Vec::new();
        if component.installed {
          markers.push("installed");
        }
        if component.compatible == Some(false) {
          markers.push("incompatible");
        }
        if markers.is_empty() {
          component.name.clone()
        } else {
          format!("{} ({})", component.name, markers.join(", "))
        }
      })
      .collect::<Vec<_>>()
      .join("\n")
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctl_core::installations::CatalogOrigin;

  fn entry(target: &str, channel: Option<&str>, id: &str) -> CatalogEntry {
    CatalogEntry {
      bundle_id: id.into(),
      target_triple: target.into(),
      origin: channel.map(|channel| CatalogOrigin {
        channel: channel.into(),
        publication_id: Some("fixture-publication".into()),
        built_at: Some("2026-01-01T00:00:00Z".into()),
      }),
      current: channel.is_some(),
    }
  }

  fn target(target: &str, compatible: bool) -> PublishedTarget {
    PublishedTarget {
      target_triple: target.into(),
      compatible,
      components: BTreeMap::new(),
    }
  }

  #[test]
  fn installed_components_deduplicate_legacy_builds_and_channels() {
    let entries = vec![
      entry("x86_64-unknown-linux-musl", Some("stable"), "stable-build"),
      entry("aarch64-apple-darwin", None, "legacy-build"),
      entry("aarch64-apple-darwin", Some("nightly"), "nightly-build"),
      entry("aarch64-apple-darwin", Some("stable"), "stable-build"),
    ];
    let components = installed(&entries, None);
    assert_eq!(
      components.render(),
      "bundle-aarch64-apple-darwin (installed)\nbundle-x86_64-unknown-linux-musl (installed)"
    );
    assert_eq!(
      serde_json::to_value(&components).unwrap(),
      serde_json::json!({
        "schema_version": 1,
        "components": [
          {"name": "bundle-aarch64-apple-darwin", "target_triple": "aarch64-apple-darwin", "installed": true},
          {"name": "bundle-x86_64-unknown-linux-musl", "target_triple": "x86_64-unknown-linux-musl", "installed": true},
        ],
      })
    );
    assert_eq!(
      installed(&entries, Some("nightly")).render(),
      "bundle-aarch64-apple-darwin (installed)"
    );
    assert_eq!(
      installed(&entries, Some("local")).render(),
      "No components."
    );
  }

  #[test]
  fn available_components_use_scoped_registrations_and_keep_unpublished_targets() {
    let mut removed = entry("aarch64-apple-darwin", Some("stable"), "removed-build");
    removed.current = false;
    let entries = vec![
      entry("aarch64-apple-darwin", None, "legacy-build"),
      entry("aarch64-apple-darwin", Some("nightly"), "nightly-build"),
      entry("x86_64-apple-darwin", Some("stable"), "stable-build"),
      removed,
    ];
    let components = available(
      vec![
        target("x86_64-unknown-linux-musl", false),
        target("aarch64-apple-darwin", true),
        target("aarch64-apple-darwin", true),
      ],
      &entries,
      "stable",
    );
    assert_eq!(
      components.render(),
      "bundle-aarch64-apple-darwin\nbundle-x86_64-apple-darwin (installed)\nbundle-x86_64-unknown-linux-musl (incompatible)"
    );
    assert_eq!(
      serde_json::to_value(&components).unwrap(),
      serde_json::json!({
        "schema_version": 1,
        "components": [
          {"name": "bundle-aarch64-apple-darwin", "target_triple": "aarch64-apple-darwin", "installed": false, "compatible": true},
          {"name": "bundle-x86_64-apple-darwin", "target_triple": "x86_64-apple-darwin", "installed": true},
          {"name": "bundle-x86_64-unknown-linux-musl", "target_triple": "x86_64-unknown-linux-musl", "installed": false, "compatible": false},
        ],
      })
    );
  }

  #[test]
  fn published_installed_components_have_one_marked_entry() {
    let components = available(
      vec![target("aarch64-apple-darwin", true)],
      &[entry(
        "aarch64-apple-darwin",
        Some("stable"),
        "stable-build",
      )],
      "stable",
    );
    assert_eq!(components.components.len(), 1);
    assert_eq!(
      components.render(),
      "bundle-aarch64-apple-darwin (installed)"
    );
    assert_eq!(components.components[0].compatible, Some(true));
  }
}
