//! Read-only publication discovery, warmed at startup without delaying the UI.

use ctl_client::remote_bundle::published::{self, PublishedCatalog};
use ctl_core::component_update::BundleChannel;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckRequest {
  channel: Option<BundleChannel>,
  #[serde(default)]
  refresh: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct CatalogSnapshot {
  default_channel: BundleChannel,
  local_development: bool,
  catalog: Option<PublishedCatalog>,
  error: Option<String>,
}

struct Cached {
  checked: Instant,
  result: Result<PublishedCatalog, String>,
}

fn cache(channel: BundleChannel) -> &'static Mutex<Option<Cached>> {
  static CACHE: OnceLock<[Mutex<Option<Cached>>; 2]> = OnceLock::new();
  let index = match channel {
    BundleChannel::Dev => 0,
    BundleChannel::Stable => 1,
  };
  &CACHE.get_or_init(|| [Mutex::new(None), Mutex::new(None)])[index]
}

async fn snapshot(channel: BundleChannel, refresh: bool) -> CatalogSnapshot {
  // Serialize simultaneous startup/dialog requests for the same channel.
  let mut cached = cache(channel).lock().await;
  if refresh
    || cached
      .as_ref()
      .is_none_or(|value| value.checked.elapsed() > Duration::from_mins(5))
  {
    let result = published::check(channel)
      .await
      .map_err(|error| error.to_string());
    *cached = Some(Cached {
      checked: Instant::now(),
      result,
    });
  }
  let result = &cached
    .as_ref()
    .expect("publication check was completed")
    .result;
  CatalogSnapshot {
    default_channel: published::default_channel(),
    local_development: local_development(),
    catalog: result.as_ref().ok().cloned(),
    error: result.as_ref().err().cloned(),
  }
}

fn local_development() -> bool {
  #[cfg(unix)]
  {
    ctl_client::component_update::working_tree_available()
  }
  #[cfg(not(unix))]
  false
}

pub(crate) fn start() {
  tauri::async_runtime::spawn(async {
    let _ = snapshot(published::default_channel(), false).await;
  });
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn check_component_updates(request: CheckRequest) -> CatalogSnapshot {
  snapshot(
    request.channel.unwrap_or_else(published::default_channel),
    request.refresh,
  )
  .await
}
