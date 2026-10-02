//! Credential-free projection of saved profiles and observed VPN connections.

use ctl_client::hosts::SavedVpnDocument;
use ctl_ipc::{VpnConnection, VpnProvider, VpnSettings, VpnSnapshot, VpnState, VpnStatus};

#[derive(Debug, serde::Serialize)]
pub(super) struct Entry {
  pub(super) vpn_id: Option<String>,
  pub(super) connection_id: Option<String>,
  pub(super) name: Option<String>,
  pub(super) provider: VpnProvider,
  pub(super) saved: bool,
  pub(super) state: State,
  pub(super) locally_connected: Option<bool>,
  pub(super) server: Option<String>,
  pub(super) username: Option<String>,
  pub(super) endpoint: Option<String>,
  pub(super) hostname: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
  Disconnected,
  Starting,
  Connected,
  Stopping,
  SignInRequired,
  Unavailable,
}

impl State {
  fn label(self) -> &'static str {
    match self {
      Self::Disconnected => "disconnected",
      Self::Starting => "starting",
      Self::Connected => "connected",
      Self::Stopping => "stopping",
      Self::SignInRequired => "sign-in required",
      Self::Unavailable => "unavailable",
    }
  }
}

pub(super) fn entries(document: Option<&SavedVpnDocument>, snapshot: &VpnSnapshot) -> Vec<Entry> {
  let mut entries = Vec::new();
  let mut matched = vec![false; snapshot.connections.len()];
  if let Some(document) = document {
    for profile in &document.connections {
      let mut observed = false;
      for (index, runtime) in snapshot.connections.iter().enumerate() {
        if matches_profile(profile, runtime) {
          entries.push(entry(Some(profile), Some(runtime), snapshot));
          matched[index] = true;
          observed = true;
        }
      }
      if !observed {
        entries.push(entry(Some(profile), None, snapshot));
      }
    }
  }
  for (runtime, matched) in snapshot.connections.iter().zip(matched) {
    if !matched {
      entries.push(entry(None, Some(runtime), snapshot));
    }
  }
  entries
}

fn matches_profile(profile: &VpnConnection, runtime: &VpnStatus) -> bool {
  if profile.provider() != runtime.provider {
    return false;
  }
  let id = runtime.connection_id.as_deref().or_else(|| {
    // Shared containers explicitly omit connection_id for env-file starts.
    // Only legacy statuses may need their runtime ID used as a profile ID.
    (!runtime.shared_container)
      .then_some(runtime.vpn_id.as_deref())
      .flatten()
  });
  id == Some(profile.connection_id.as_str())
}

fn entry(
  profile: Option<&VpnConnection>,
  runtime: Option<&VpnStatus>,
  snapshot: &VpnSnapshot,
) -> Entry {
  let provider = runtime.map_or_else(
    || {
      profile
        .expect("an inventory entry has a profile or runtime")
        .provider()
    },
    |runtime| runtime.provider,
  );
  let saved_id = profile.map(|profile| profile.connection_id.as_str());
  let (saved_server, saved_username, saved_hostname) =
    match profile.map(|profile| &profile.settings) {
      Some(VpnSettings::Openconnect { url, username, .. }) => {
        (Some(url.as_str()), Some(username.as_str()), None)
      }
      Some(VpnSettings::Tailscale { hostname, .. }) => (None, None, hostname.as_deref()),
      None => (None, None, None),
    };
  let hostname = runtime
    .and_then(|runtime| runtime.hostname.as_deref())
    .or(saved_hostname)
    .map(str::to_owned);
  let server = match provider {
    VpnProvider::Openconnect => runtime
      .and_then(|runtime| runtime.vpn_url.as_deref())
      .or(saved_server)
      .and_then(gateway_origin),
    VpnProvider::Tailscale => runtime
      .and_then(|runtime| runtime.tailnet.as_deref())
      .map(str::to_owned)
      .or_else(|| hostname.clone()),
  };
  let state = runtime.map_or_else(
    || {
      // Legacy replies are local-only and cannot rule out a shared container.
      if snapshot.supports_multiple && snapshot.discovery_warnings.is_empty() {
        State::Disconnected
      } else {
        State::Unavailable
      }
    },
    |runtime| {
      if runtime.status_unavailable {
        return State::Unavailable;
      }
      match runtime.state {
        VpnState::Stopped => State::Disconnected,
        VpnState::Starting if super::sign_in_url(runtime).is_some() => State::SignInRequired,
        VpnState::Starting => State::Starting,
        VpnState::Connected => State::Connected,
        VpnState::Stopping => State::Stopping,
      }
    },
  );
  Entry {
    vpn_id: runtime
      .and_then(|runtime| {
        runtime
          .vpn_id
          .as_deref()
          .or(runtime.connection_id.as_deref())
      })
      .or(saved_id)
      .map(str::to_owned),
    connection_id: runtime
      .and_then(|runtime| runtime.connection_id.as_deref())
      .or(saved_id)
      .map(str::to_owned),
    name: profile.map(|profile| profile.name.clone()),
    provider,
    saved: profile.is_some(),
    state,
    locally_connected: runtime.and_then(|runtime| runtime.locally_connected),
    server,
    username: runtime
      .and_then(|runtime| runtime.username.as_deref())
      .or(saved_username)
      .map(str::to_owned),
    endpoint: runtime
      .filter(|runtime| !runtime.status_unavailable && runtime.state == VpnState::Connected)
      .and_then(|runtime| runtime.endpoint.clone()),
    hostname,
  }
}

fn gateway_origin(value: &str) -> Option<String> {
  let value = if value.contains("://") {
    value.to_owned()
  } else {
    format!("https://{value}")
  };
  let url = url::Url::parse(&value).ok()?;
  if url.scheme() != "https" || url.host_str().is_none() {
    return None;
  }
  Some(url.origin().ascii_serialization())
}

pub(super) fn format(entries: &[Entry]) -> String {
  if entries.is_empty() {
    return "No saved VPN profiles or runtime connections.".into();
  }
  let headers = [
    "NAME",
    "PROVIDER",
    "STATE",
    "SERVER",
    "USERNAME",
    "SOCKS5 ENDPOINT",
    "VPN ID",
  ];
  let rows = entries.iter().map(|entry| {
    let provider = match entry.provider {
      VpnProvider::Openconnect => "OpenConnect",
      VpnProvider::Tailscale => "Tailscale",
    };
    [
      value(entry.name.as_deref()),
      provider.into(),
      entry.state.label().into(),
      value(entry.server.as_deref()),
      value(entry.username.as_deref()),
      value(entry.endpoint.as_deref()),
      value(entry.vpn_id.as_deref()),
    ]
  });
  if entries
    .iter()
    .any(|entry| entry.locally_connected == Some(false))
  {
    let rows = rows.zip(entries).map(
      |([name, provider, state, server, username, endpoint, id], entry)| {
        [
          name,
          provider,
          state,
          super::usage_label(entry.locally_connected).into(),
          server,
          username,
          endpoint,
          id,
        ]
      },
    );
    crate::table::format(
      [
        "NAME",
        "PROVIDER",
        "STATE",
        "USE",
        "SERVER",
        "USERNAME",
        "SOCKS5 ENDPOINT",
        "VPN ID",
      ],
      rows,
    )
  } else {
    crate::table::format(headers, rows)
  }
}

fn value(value: Option<&str>) -> String {
  value
    .filter(|value| !value.is_empty())
    .unwrap_or("-")
    .into()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn profile(id: &str) -> VpnConnection {
    VpnConnection {
      connection_id: id.into(),
      name: format!("Saved {id}"),
      settings: VpnSettings::Openconnect {
        url: "saved.example.test/private-group?token=saved-token".into(),
        username: "saved-user".into(),
        password: zeroize::Zeroizing::new("saved-password".into()),
        auth_method: None,
        target_ip: None,
      },
    }
  }

  fn runtime(id: &str) -> VpnStatus {
    VpnStatus {
      vpn_id: Some(format!("runtime-{id}")),
      connection_id: Some(id.into()),
      provider: VpnProvider::Openconnect,
      state: VpnState::Connected,
      running: true,
      locally_connected: Some(true),
      endpoint: Some("socks5h://127.0.0.1:49152".into()),
      ..VpnStatus::default()
    }
  }

  fn document(connections: Vec<VpnConnection>) -> SavedVpnDocument {
    SavedVpnDocument {
      connections,
      ..SavedVpnDocument::default()
    }
  }

  fn snapshot(connections: Vec<VpnStatus>) -> VpnSnapshot {
    VpnSnapshot {
      connections,
      ..VpnSnapshot::default()
    }
  }

  #[test]
  fn matches_profile_ids_without_changing_the_runtime_stop_id() {
    let rows = entries(
      Some(&document(vec![profile("work")])),
      &snapshot(vec![runtime("work")]),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].saved);
    assert_eq!(rows[0].vpn_id.as_deref(), Some("runtime-work"));
    assert_eq!(rows[0].connection_id.as_deref(), Some("work"));
    assert_eq!(rows[0].name.as_deref(), Some("Saved work"));
    assert_eq!(rows[0].state, State::Connected);
  }

  #[test]
  fn only_legacy_statuses_can_fall_back_to_the_vpn_id() {
    let legacy = VpnStatus {
      vpn_id: Some("work".into()),
      connection_id: None,
      ..runtime("work")
    };
    let shared_env = VpnStatus {
      shared_container: true,
      ..legacy.clone()
    };
    let other_profile = VpnStatus {
      connection_id: Some("other".into()),
      ..legacy.clone()
    };
    let rows = entries(
      Some(&document(vec![profile("work")])),
      &snapshot(vec![legacy, shared_env, other_profile]),
    );
    assert_eq!(rows.len(), 3);
    assert!(rows[0].saved);
    assert!(!rows[1].saved);
    assert!(!rows[2].saved);
    assert_eq!(rows[1].connection_id, None);
    assert_eq!(rows[2].connection_id.as_deref(), Some("other"));
  }

  #[test]
  fn keeps_provider_conflicts_as_separate_entries() {
    let observed = VpnStatus {
      provider: VpnProvider::Tailscale,
      hostname: Some("observed-device".into()),
      ..runtime("work")
    };
    let rows = entries(
      Some(&document(vec![profile("work")])),
      &snapshot(vec![observed]),
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].provider, VpnProvider::Openconnect);
    assert_eq!(rows[0].state, State::Disconnected);
    assert_eq!(rows[1].provider, VpnProvider::Tailscale);
    assert!(!rows[1].saved);
    assert_eq!(rows[1].server.as_deref(), Some("observed-device"));
  }

  #[test]
  fn preserves_distinct_runtime_records_and_saved_order() {
    let second_container = VpnStatus {
      vpn_id: Some("second-container".into()),
      container_id: Some("b".repeat(64)),
      ..runtime("work")
    };
    let first_container = VpnStatus {
      container_id: Some("a".repeat(64)),
      ..runtime("work")
    };
    let rows = entries(
      Some(&document(vec![profile("work"), profile("research")])),
      &snapshot(vec![runtime("external"), first_container, second_container]),
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].vpn_id.as_deref(), Some("runtime-work"));
    assert_eq!(rows[1].vpn_id.as_deref(), Some("second-container"));
    assert_eq!(rows[2].name.as_deref(), Some("Saved research"));
    assert_eq!(rows[2].state, State::Disconnected);
    assert_eq!(rows[3].vpn_id.as_deref(), Some("runtime-external"));
    assert_eq!(rows[3].name, None);
  }

  #[test]
  fn distinguishes_missing_inventory_from_disconnected_and_hides_stale_endpoints() {
    let unavailable = VpnStatus {
      status_unavailable: true,
      ..runtime("stale")
    };
    let stopped = VpnStatus {
      state: VpnState::Stopped,
      ..runtime("stopped")
    };
    let observed = VpnSnapshot {
      discovery_warnings: vec!["Container inventory unavailable".into()],
      ..snapshot(vec![runtime("healthy"), unavailable, stopped])
    };
    let rows = entries(
      Some(&document(vec![
        profile("missing"),
        profile("healthy"),
        profile("stale"),
      ])),
      &observed,
    );
    assert_eq!(rows[0].state, State::Unavailable);
    assert_eq!(rows[0].endpoint, None);
    assert_eq!(rows[1].state, State::Connected);
    assert!(rows[1].endpoint.is_some());
    assert_eq!(rows[2].state, State::Unavailable);
    assert_eq!(rows[2].endpoint, None);
    assert_eq!(rows[3].state, State::Disconnected);
    assert_eq!(rows[3].endpoint, None);
  }

  #[test]
  fn legacy_local_only_inventory_cannot_prove_other_profiles_disconnected() {
    let observed = VpnSnapshot {
      supports_multiple: false,
      ..snapshot(vec![runtime("healthy")])
    };
    let rows = entries(
      Some(&document(vec![profile("healthy"), profile("missing")])),
      &observed,
    );
    assert_eq!(rows[0].state, State::Connected);
    assert!(rows[0].endpoint.is_some());
    assert_eq!(rows[1].state, State::Unavailable);
    assert_eq!(rows[1].endpoint, None);
  }

  #[test]
  fn sanitizes_saved_and_runtime_urls_without_exposing_settings() {
    let observed = VpnStatus {
      vpn_url: Some("https://url-user:url-password@runtime.example.test/private-path?token=runtime-token#fragment".into()),
      username: Some("runtime-user".into()),
      ..runtime("work")
    };
    let rows = entries(
      Some(&document(vec![profile("work"), profile("research")])),
      &snapshot(vec![observed]),
    );
    assert_eq!(
      rows[0].server.as_deref(),
      Some("https://runtime.example.test")
    );
    assert_eq!(rows[0].username.as_deref(), Some("runtime-user"));
    assert_eq!(
      rows[1].server.as_deref(),
      Some("https://saved.example.test")
    );
    let serialized = serde_json::to_string(&rows).unwrap();
    let rendered = format(&rows);
    for hidden in [
      "saved-password",
      "url-user",
      "url-password",
      "private-group",
      "private-path",
      "saved-token",
      "runtime-token",
      "fragment",
    ] {
      assert!(!serialized.contains(hidden));
      assert!(!rendered.contains(hidden));
    }
    assert_eq!(gateway_origin("http://gateway.example.test/token"), None);
    assert_eq!(gateway_origin("https://"), None);
    assert_eq!(gateway_origin("ftp://gateway.example.test"), None);
  }

  #[test]
  fn supports_legacy_saved_documents_and_tailscale_sign_in() {
    let legacy: SavedVpnDocument = serde_json::from_value(serde_json::json!({
      "schema_version": 1,
      "connections": [{
        "connection_id": "legacy",
        "name": "Legacy profile",
        "url": "legacy.example.test/private?token=legacy-token",
        "username": "legacy-user",
        "password": "legacy-password"
      }]
    }))
    .unwrap();
    legacy.validate().unwrap();
    let legacy_rows = entries(Some(&legacy), &snapshot(Vec::new()));
    assert_eq!(
      legacy_rows[0].server.as_deref(),
      Some("https://legacy.example.test")
    );
    let tailscale = VpnConnection {
      connection_id: "tailnet".into(),
      name: "Tailnet".into(),
      settings: VpnSettings::Tailscale {
        hostname: Some("saved-device".into()),
        accept_routes: true,
      },
    };
    let observed = VpnStatus {
      provider: VpnProvider::Tailscale,
      hostname: Some("observed-device".into()),
      tailnet: Some("tailnet.example.test".into()),
      state: VpnState::Starting,
      auth_url: Some("https://login.tailscale.com/a/example".into()),
      ..runtime("tailnet")
    };
    let rows = entries(Some(&document(vec![tailscale])), &snapshot(vec![observed]));
    assert_eq!(rows[0].state, State::SignInRequired);
    assert_eq!(rows[0].hostname.as_deref(), Some("observed-device"));
    assert_eq!(rows[0].server.as_deref(), Some("tailnet.example.test"));
    assert_eq!(rows[0].endpoint, None);
    assert_eq!(rows[0].username, None);
    let serialized = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(serialized["state"], "sign_in_required");
    assert!(serialized.get("auth_url").is_none());
  }

  #[test]
  fn requires_a_valid_tailscale_sign_in_link_before_claiming_sign_in_is_required() {
    let invalid_link = VpnStatus {
      provider: VpnProvider::Tailscale,
      state: VpnState::Starting,
      auth_url: Some("https://other.example.test/a/token".into()),
      ..runtime("invalid-link")
    };
    let wrong_provider = VpnStatus {
      state: VpnState::Starting,
      auth_url: Some("https://login.tailscale.com/a/example".into()),
      ..runtime("wrong-provider")
    };
    let rows = entries(None, &snapshot(vec![invalid_link, wrong_provider]));
    assert_eq!(rows[0].state, State::Starting);
    assert_eq!(rows[1].state, State::Starting);
    assert_eq!(rows[0].endpoint, None);
    assert_eq!(rows[1].endpoint, None);
  }
}
