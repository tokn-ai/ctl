//! Executables and dependency edges in public and managed component bundles.

use crate::component::ComponentInfo;
use std::collections::BTreeMap;

pub const MAX_FILE_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
pub const MAX_TOTAL_BYTES: usize = 512 * 1024 * 1024;

pub const SERVICES: [&str; 4] = ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld"];
pub const ALL: [&str; 6] = ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld", "ctl", "ctmux"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
  Services,
  WithClients,
}

impl Layout {
  #[must_use]
  pub const fn public(schema: u32) -> Option<Self> {
    match schema {
      1 | 2 => Some(Self::Services),
      3 => Some(Self::WithClients),
      _ => None,
    }
  }

  #[must_use]
  pub const fn managed(schema: u16) -> Option<Self> {
    match schema {
      1 => Some(Self::Services),
      2 => Some(Self::WithClients),
      _ => None,
    }
  }

  #[must_use]
  pub const fn managed_schema(self) -> u16 {
    match self {
      Self::Services => 1,
      Self::WithClients => 2,
    }
  }

  #[must_use]
  pub const fn names(self) -> &'static [&'static str] {
    match self {
      Self::Services => &SERVICES,
      Self::WithClients => &ALL,
    }
  }

  #[must_use]
  pub fn matches<T>(self, components: &BTreeMap<String, T>) -> bool {
    components.len() == self.names().len()
      && self
        .names()
        .iter()
        .all(|name| components.contains_key(*name))
  }

  #[must_use]
  pub fn from_components<T>(components: &BTreeMap<String, T>) -> Option<Self> {
    [Self::Services, Self::WithClients]
      .into_iter()
      .find(|layout| layout.matches(components))
  }

  /// Check the contracts between the executables in this bundle, independently
  /// of the contracts supported by the machine inspecting the bundle.
  #[must_use]
  pub fn compatible(self, components: &BTreeMap<String, ComponentInfo>) -> bool {
    let services = [
      ("ctl-agent", "ctld", "ctld"),
      ("ctl-agent", "ctmuxd", "ctmux"),
      ("ctl-agent", "ctmuxd", "ctmux_control"),
      ("ctl-agent", "ctl-taskd", "task"),
      ("ctl-agent", "ctl-taskd", "task_control"),
      ("ctl-taskd", "ctmuxd", "ctmux"),
      ("ctl-taskd", "ctmuxd", "ctmux_control"),
    ];
    let clients = [
      ("ctl", "ctl-agent", "ctl_identity"),
      ("ctl", "ctl-agent", "ctl_maintenance"),
      ("ctl", "ctl-agent", "ctl_remote_vpn"),
      ("ctl", "ctld", "ctld"),
      ("ctl", "ctld", "ctld_lifecycle"),
      ("ctl", "ctld", "ctld_helper"),
      ("ctl", "ctmuxd", "ctmux"),
      ("ctl", "ctmuxd", "ctmux_control"),
      ("ctl", "ctl-taskd", "task"),
      ("ctl", "ctl-taskd", "task_control"),
      ("ctmux", "ctmuxd", "ctmux"),
      ("ctmux", "ctmuxd", "ctmux_control"),
    ];
    let clients = if self == Self::WithClients {
      &clients[..]
    } else {
      &[]
    };
    services
      .iter()
      .chain(clients)
      .all(|(consumer, provider, name)| {
        let protocol = |id: &str| {
          components
            .get(id)?
            .protocols
            .iter()
            .find(|p| p.name == *name)
        };
        protocol(consumer)
          .zip(protocol(provider))
          .is_some_and(|(client, server)| server.negotiate(&client.supported_versions).is_some())
      })
  }
}

/// Advertisements a public bundle must supply, including contracts used by
/// clients outside this bundle.
#[must_use]
pub fn required_protocols(component: &str) -> &'static [&'static str] {
  match component {
    "ctl-agent" => &[
      "ctl_identity",
      "ctl_maintenance",
      "ctl_remote_vpn",
      "ctld",
      "ctmux",
      "ctmux_control",
      "task",
      "task_control",
    ],
    "ctmuxd" | "ctmux" => &["ctmux", "ctmux_control"],
    "ctl-taskd" => &["task", "task_control", "ctmux", "ctmux_control"],
    "ctld" => &["ctld", "ctld_lifecycle", "ctld_helper"],
    "ctl" => &[
      "ctl_identity",
      "ctl_maintenance",
      "ctl_remote_vpn",
      "ctld",
      "ctld_lifecycle",
      "ctld_helper",
      "ctmux",
      "ctmux_control",
      "task",
      "task_control",
    ],
    _ => &[],
  }
}
