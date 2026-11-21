//! Logical component names remain stable when installed builds change.

use ctl_client::remote_bundle::published::PublishedTarget;
use ctl_core::installations::CatalogEntry;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io;

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
  #[serde(skip_serializing_if = "Option::is_none")]
  compatible: Option<bool>,
}

pub(super) fn installed(
  entries: &[CatalogEntry],
  channel: Option<&str>,
) -> io::Result<ComponentList> {
  let memberships = memberships(entries)?;
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
  Ok(ComponentList {
    schema_version: 1,
    components: targets
      .into_iter()
      .map(|target| Component::new(target.into(), memberships.get(target), None))
      .collect(),
  })
}

pub(super) fn available(
  targets: Vec<PublishedTarget>,
  entries: &[CatalogEntry],
) -> io::Result<ComponentList> {
  let memberships = memberships(entries)?;
  let mut components = memberships
    .keys()
    .map(|target| (target.clone(), None))
    .collect::<BTreeMap<_, _>>();
  components.extend(
    targets
      .into_iter()
      .map(|target| (target.target_triple, Some(target.compatible))),
  );
  Ok(ComponentList {
    schema_version: 1,
    components: components
      .into_iter()
      .map(|(target, compatible)| {
        let membership = memberships.get(&target);
        Component::new(target, membership, compatible)
      })
      .collect(),
  })
}

fn memberships(entries: &[CatalogEntry]) -> io::Result<BTreeMap<String, BTreeSet<String>>> {
  let mut memberships = BTreeMap::<String, BTreeSet<String>>::new();
  for entry in entries {
    let origin = entry.origin.as_ref().ok_or_else(metadata_unavailable)?;
    memberships
      .entry(entry.target_triple.clone())
      .or_default()
      .insert(origin.channel.clone());
  }
  Ok(memberships)
}

pub(super) fn metadata_unavailable() -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidData,
    "component channel metadata is unavailable; update ctlup on the target host",
  )
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
  fn new(
    target_triple: String,
    membership: Option<&BTreeSet<String>>,
    compatible: Option<bool>,
  ) -> Self {
    let mut channels = membership
      .map(|channels| channels.iter().cloned().collect::<Vec<_>>())
      .unwrap_or_default();
    channels.sort_by(|left, right| channel_order(left).cmp(&channel_order(right)));
    Self {
      name: format!("bundle-{target_triple}"),
      target_triple,
      installed: membership.is_some(),
      channels,
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
        if !component.channels.is_empty() {
          line.push_str(" (");
          line.push_str(&component.channels.join(", "));
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

  fn entry(target: &str, channel: &str, id: &str) -> CatalogEntry {
    CatalogEntry {
      bundle_id: id.into(),
      target_triple: target.into(),
      origin: Some(CatalogOrigin {
        channel: channel.into(),
        publication_id: Some("fixture-publication".into()),
        built_at: Some("2026-01-01T00:00:00Z".into()),
      }),
      current: true,
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
  fn installed_components_deduplicate_builds_and_channels() {
    let mut retained = entry("aarch64-apple-darwin", "stable", "retained-build");
    retained.current = false;
    let entries = vec![
      entry("x86_64-unknown-linux-musl", "provided", "provided-build"),
      entry("aarch64-apple-darwin", "nightly", "nightly-build"),
      entry("aarch64-apple-darwin", "stable", "stable-build"),
      retained,
    ];
    let components = installed(&entries, None).unwrap();
    assert_eq!(
      components.render(),
      "bundle-aarch64-apple-darwin (stable, nightly)\nbundle-x86_64-unknown-linux-musl (provided)"
    );
    assert_eq!(
      serde_json::to_value(&components).unwrap(),
      serde_json::json!({
        "schema_version": 1,
        "components": [
          {"name": "bundle-aarch64-apple-darwin", "target_triple": "aarch64-apple-darwin", "installed": true, "channels": ["stable", "nightly"]},
          {"name": "bundle-x86_64-unknown-linux-musl", "target_triple": "x86_64-unknown-linux-musl", "installed": true, "channels": ["provided"]},
        ],
      })
    );
    assert_eq!(
      installed(&entries, Some("nightly")).unwrap().render(),
      "bundle-aarch64-apple-darwin (stable, nightly)"
    );
    assert_eq!(
      installed(&entries, Some("local")).unwrap().render(),
      "No components."
    );
  }

  #[test]
  fn available_components_include_retained_other_channels_and_unpublished_targets() {
    let mut retained = entry("x86_64-apple-darwin", "nightly", "retained-build");
    retained.current = false;
    let entries = vec![
      entry("aarch64-apple-darwin", "local", "local-build"),
      entry(
        "aarch64-unknown-linux-musl",
        "provided",
        "unpublished-build",
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
    )
    .unwrap();
    assert_eq!(
      components.render(),
      "bundle-aarch64-apple-darwin (local)\nbundle-aarch64-unknown-linux-musl (provided)\nbundle-x86_64-apple-darwin (nightly)\nbundle-x86_64-unknown-linux-musl (incompatible)"
    );
    assert_eq!(
      serde_json::to_value(&components).unwrap(),
      serde_json::json!({
        "schema_version": 1,
        "components": [
          {"name": "bundle-aarch64-apple-darwin", "target_triple": "aarch64-apple-darwin", "installed": true, "channels": ["local"], "compatible": true},
          {"name": "bundle-aarch64-unknown-linux-musl", "target_triple": "aarch64-unknown-linux-musl", "installed": true, "channels": ["provided"]},
          {"name": "bundle-x86_64-apple-darwin", "target_triple": "x86_64-apple-darwin", "installed": true, "channels": ["nightly"]},
          {"name": "bundle-x86_64-unknown-linux-musl", "target_triple": "x86_64-unknown-linux-musl", "installed": false, "channels": [], "compatible": false},
        ],
      })
    );
  }

  #[test]
  fn published_installed_components_have_one_marked_entry() {
    let components = available(
      vec![target("aarch64-apple-darwin", false)],
      &[entry("aarch64-apple-darwin", "stable", "stable-build")],
    )
    .unwrap();
    assert_eq!(components.components.len(), 1);
    assert_eq!(
      components.render(),
      "bundle-aarch64-apple-darwin (stable) (incompatible)"
    );
    assert_eq!(components.components[0].compatible, Some(false));
  }

  #[test]
  fn scoped_target_selection_keeps_all_memberships_in_stable_order() {
    let entries = [
      "pr-2", "provided", "local", "nightly", "pr-12", "stable", "stable",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, channel)| entry("aarch64-apple-darwin", channel, &format!("build-{index}")))
    .collect::<Vec<_>>();
    let components = installed(&entries, Some("provided")).unwrap();
    assert_eq!(
      components.render(),
      "bundle-aarch64-apple-darwin (stable, nightly, local, provided, pr-12, pr-2)"
    );
    assert_eq!(
      components.components[0].channels,
      ["stable", "nightly", "local", "provided", "pr-12", "pr-2"]
    );
    assert!(components.components[0].installed);
  }

  #[test]
  fn unavailable_channel_metadata_requires_a_newer_peer() {
    let mut unknown = entry("aarch64-apple-darwin", "provided", "unknown-build");
    unknown.origin = None;
    unknown.current = false;
    for result in [
      installed(std::slice::from_ref(&unknown), None),
      installed(std::slice::from_ref(&unknown), Some("stable")),
      available(vec![target("aarch64-apple-darwin", true)], &[unknown]),
    ] {
      let error = result.unwrap_err();
      assert_eq!(error.kind(), io::ErrorKind::InvalidData);
      assert!(
        error
          .to_string()
          .contains("component channel metadata is unavailable")
      );
    }
  }
}
