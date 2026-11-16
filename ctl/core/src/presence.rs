//! Nonsecret, client-reported labels for live service and terminal connections.

use serde::{Deserialize, Serialize};

#[cfg(feature = "presence")]
mod registry;
#[cfg(feature = "presence")]
pub use registry::{ConnectionRegistration, connections, connections_in};

/// One live inbound ctl service channel. Several can belong to the same app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConnection {
  pub connection_id: String,
  pub client: Option<ClientIdentity>,
  pub service: String,
  pub connected_at_ms: u64,
  pub source: Option<String>,
}

/// A client process, shared by its service channels and pane attachments.
/// Labels describe the client; they are not an authenticated human identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientIdentity {
  pub instance_id: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub device_name: Option<String>,
  pub client_name: String,
  pub client_version: String,
}

impl ClientIdentity {
  /// Reject unbounded or terminal-control-bearing peer metadata.
  #[must_use]
  pub fn validate(&self) -> bool {
    valid_label(&self.instance_id, 128)
      && valid_label(&self.client_name, 64)
      && valid_label(&self.client_version, 64)
      && self
        .device_name
        .as_deref()
        .is_none_or(|value| valid_label(value, 128))
  }
}

fn valid_label(value: &str, limit: usize) -> bool {
  !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

/// Escapes terminal controls, bidi directives and Unicode line separators,
/// preserving ordinary Unicode names and avoiding changes to JSON data.
#[must_use]
pub fn display_label(value: &str) -> String {
  let mut rendered = String::with_capacity(value.len());
  for character in value.chars() {
    if character.is_control()
      || matches!(character, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    {
      rendered.extend(character.escape_default());
    } else {
      rendered.push(character);
    }
  }
  rendered
}

/// One process identity is retained across reconnects and all of its panes.
/// `CTL_DEVICE_NAME` can provide a display label instead of the OS node name.
#[cfg(feature = "presence")]
#[must_use]
pub fn local_identity(client_name: &str, client_version: &str) -> ClientIdentity {
  use std::sync::OnceLock;
  static INSTANCE_ID: OnceLock<String> = OnceLock::new();
  static DEVICE_NAME: OnceLock<Option<String>> = OnceLock::new();
  ClientIdentity {
    instance_id: INSTANCE_ID
      .get_or_init(|| uuid::Uuid::new_v4().to_string())
      .clone(),
    device_name: DEVICE_NAME.get_or_init(device_name).clone(),
    client_name: bounded_label(client_name, 64).unwrap_or_else(|| "ctl-client".into()),
    client_version: bounded_label(client_version, 64).unwrap_or_else(|| "unknown".into()),
  }
}

/// Identifies the current application when a lower transport has no UI context.
#[cfg(feature = "presence")]
#[must_use]
pub fn process_identity(client_version: &str) -> ClientIdentity {
  let executable = std::env::current_exe().ok();
  let name = executable
    .as_deref()
    .and_then(std::path::Path::file_stem)
    .and_then(std::ffi::OsStr::to_str)
    .unwrap_or("ctl-client");
  local_identity(name, client_version)
}

#[cfg(feature = "presence")]
fn device_name() -> Option<String> {
  if let Ok(value) = std::env::var("CTL_DEVICE_NAME") {
    return bounded_label(&value, 128);
  }
  #[cfg(unix)]
  let value = rustix::system::uname()
    .nodename()
    .to_string_lossy()
    .into_owned();
  #[cfg(not(unix))]
  let value = std::env::var("COMPUTERNAME").unwrap_or_default();
  bounded_label(&value, 128)
}

/// Removes terminal control characters and bounds UTF-8 display labels by bytes.
#[must_use]
pub fn bounded_label(value: &str, limit: usize) -> Option<String> {
  let mut result = String::new();
  for character in value.trim().chars().filter(|value| !value.is_control()) {
    if result.len() + character.len_utf8() > limit {
      break;
    }
    result.push(character);
  }
  (!result.trim().is_empty()).then_some(result)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn identity() -> ClientIdentity {
    ClientIdentity {
      instance_id: "fixture-instance".into(),
      device_name: Some("Fixture device".into()),
      client_name: "ctmux-app".into(),
      client_version: "0.1.2".into(),
    }
  }

  #[test]
  fn peer_labels_are_bounded_and_cannot_inject_terminal_controls() {
    assert!(identity().validate());
    for value in ["", " ", "line\nbreak", "\u{1b}[31m", "\u{7f}"] {
      let mut peer = identity();
      peer.device_name = Some(value.into());
      assert!(!peer.validate());
    }
    let mut peer = identity();
    peer.instance_id = "x".repeat(129);
    assert!(!peer.validate());
    peer = identity();
    peer.client_name = "x".repeat(65);
    assert!(!peer.validate());
  }

  #[test]
  fn human_labels_preserve_unicode_and_escape_formatting_controls() {
    assert_eq!(
      display_label("设备\u{202e}\u{2028}\n\u{1b}"),
      "设备\\u{202e}\\u{2028}\\n\\u{1b}"
    );
  }

  #[cfg(feature = "presence")]
  #[test]
  fn process_identity_is_shared_between_channels_and_utf8_labels_are_bounded() {
    let first = local_identity("ctmux-app", "0.1.2");
    let second = local_identity("ctl", "0.1.2");
    assert_eq!(first.instance_id, second.instance_id);
    assert_eq!(first.device_name, second.device_name);
    assert!(first.validate());
    assert!(second.validate());
    assert_eq!(bounded_label("\n界界\u{1b}", 5), Some("界".into()));
    assert_eq!(bounded_label("\n\t", 128), None);
  }
}
