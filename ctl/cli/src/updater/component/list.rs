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
  channels: Vec<String>,
  has_legacy: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  compatible: Option<bool>,
}

#[derive(Default)]
struct Membership {
  channels: BTreeSet<String>,
  has_legacy: bool,
}

pub(super) fn installed(entries: &[CatalogEntry], channel: Option<&str>) -> ComponentList {
  let memberships = memberships(entries);
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
      .map(|target| Component::new(target.into(), memberships.get(target), None))
      .collect(),
  }
}

pub(super) fn available(targets: Vec<PublishedTarget>, entries: &[CatalogEntry]) -> ComponentList {
  let memberships = memberships(entries);
  let mut components = memberships
    .keys()
    .map(|target| (target.clone(), None))
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
        let membership = memberships.get(&target);
        Component::new(target, membership, compatible)
      })
      .collect(),
  }
}

fn memberships(entries: &[CatalogEntry]) -> BTreeMap<String, Membership> {
  let mut memberships = BTreeMap::<String, Membership>::new();
  for entry in entries {
    let membership = memberships.entry(entry.target_triple.clone()).or_default();
    if let Some(origin) = &entry.origin {
      membership.channels.insert(origin.channel.clone());
    } else {
      membership.has_legacy = true;
    }
  }
  memberships
}

fn channel_order(channel: &str) -> (u8, &str) {
  let rank = match channel {
    "stable" => 0,
    "nightly" => 1,
    "local" => 2,
    "provided" => 3,
    _ => 4,
  };
  (rank, channel)
}

impl Component {
  fn new(target_triple: String, membership: Option<&Membership>, compatible: Option<bool>) -> Self {
    let mut channels = membership
      .map(|membership| membership.channels.iter().cloned().collect::<Vec<_>>())
      .unwrap_or_default();
    channels.sort_by(|left, right| channel_order(left).cmp(&channel_order(right)));
    Self {
      name: format!("bundle-{target_triple}"),
      target_triple,
      installed: membership.is_some(),
      channels,
      has_legacy: membership.is_some_and(|membership| membership.has_legacy),
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
        let mut line = component.name.clone();
        let mut memberships = component
          .channels
          .iter()
          .map(String::as_str)
          .collect::<Vec<_>>();
        if component.has_legacy {
          memberships.push("legacy");
        }
        if !memberships.is_empty() {
          line.push_str(" (");
          line.push_str(&memberships.join(", "));
          line.push(')');
        }
        if component.compatible == Some(false) {
          line.push_str(" (incompatible)");
        }
        line
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
    let mut retained = entry("aarch64-apple-darwin", Some("stable"), "retained-build");
    retained.current = false;
    let entries = vec![
      entry("x86_64-unknown-linux-musl", None, "legacy-build"),
      entry("aarch64-apple-darwin", Some("nightly"), "nightly-build"),
      entry("aarch64-apple-darwin", Some("stable"), "stable-build"),
      retained,
    ];
    let components = installed(&entries, None);
    assert_eq!(
      components.render(),
      "bundle-aarch64-apple-darwin (stable, nightly)\nbundle-x86_64-unknown-linux-musl (legacy)"
    );
    assert_eq!(
      serde_json::to_value(&components).unwrap(),
      serde_json::json!({
        "schema_version": 1,
        "components": [
          {"name": "bundle-aarch64-apple-darwin", "target_triple": "aarch64-apple-darwin", "installed": true, "channels": ["stable", "nightly"], "has_legacy": false},
          {"name": "bundle-x86_64-unknown-linux-musl", "target_triple": "x86_64-unknown-linux-musl", "installed": true, "channels": [], "has_legacy": true},
        ],
      })
    );
    assert_eq!(
      installed(&entries, Some("nightly")).render(),
      "bundle-aarch64-apple-darwin (stable, nightly)"
    );
    assert_eq!(
      installed(&entries, Some("local")).render(),
      "No components."
    );
  }

  #[test]
  fn available_components_include_retained_other_channels_and_unpublished_legacy_targets() {
    let mut retained = entry("x86_64-apple-darwin", Some("nightly"), "retained-build");
    retained.current = false;
    let entries = vec![
      entry("aarch64-apple-darwin", None, "legacy-build"),
      entry(
        "aarch64-unknown-linux-musl",
        None,
        "unpublished-legacy-build",
      ),
      retained,
    ];
    let components = available(
      vec![
        target("x86_64-unknown-linux-musl", false),
        target("aarch64-apple-darwin", true),
        target("aarch64-apple-darwin", true),
      ],
      &entries,
    );
    assert_eq!(
      components.render(),
      "bundle-aarch64-apple-darwin (legacy)\nbundle-aarch64-unknown-linux-musl (legacy)\nbundle-x86_64-apple-darwin (nightly)\nbundle-x86_64-unknown-linux-musl (incompatible)"
    );
    assert_eq!(
      serde_json::to_value(&components).unwrap(),
      serde_json::json!({
        "schema_version": 1,
        "components": [
          {"name": "bundle-aarch64-apple-darwin", "target_triple": "aarch64-apple-darwin", "installed": true, "channels": [], "has_legacy": true, "compatible": true},
          {"name": "bundle-aarch64-unknown-linux-musl", "target_triple": "aarch64-unknown-linux-musl", "installed": true, "channels": [], "has_legacy": true},
          {"name": "bundle-x86_64-apple-darwin", "target_triple": "x86_64-apple-darwin", "installed": true, "channels": ["nightly"], "has_legacy": false},
          {"name": "bundle-x86_64-unknown-linux-musl", "target_triple": "x86_64-unknown-linux-musl", "installed": false, "channels": [], "has_legacy": false, "compatible": false},
        ],
      })
    );
  }

  #[test]
  fn published_installed_components_have_one_marked_entry() {
    let components = available(
      vec![target("aarch64-apple-darwin", false)],
      &[entry(
        "aarch64-apple-darwin",
        Some("stable"),
        "stable-build",
      )],
    );
    assert_eq!(components.components.len(), 1);
    assert_eq!(
      components.render(),
      "bundle-aarch64-apple-darwin (stable) (incompatible)"
    );
    assert_eq!(components.components[0].compatible, Some(false));
  }

  #[test]
  fn full_memberships_have_stable_order_and_keep_legacy_separate_from_channels() {
    let mut entries = [
      "pr-2", "provided", "local", "nightly", "pr-12", "stable", "stable",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, channel)| {
      entry(
        "aarch64-apple-darwin",
        Some(channel),
        &format!("build-{index}"),
      )
    })
    .collect::<Vec<_>>();
    entries.push(entry("aarch64-apple-darwin", None, "legacy-build"));
    let components = installed(&entries, Some("provided"));
    assert_eq!(
      components.render(),
      "bundle-aarch64-apple-darwin (stable, nightly, local, provided, pr-12, pr-2, legacy)"
    );
    assert_eq!(
      components.components[0].channels,
      ["stable", "nightly", "local", "provided", "pr-12", "pr-2"]
    );
    assert!(components.components[0].has_legacy);
    assert!(components.components[0].installed);
  }
}
