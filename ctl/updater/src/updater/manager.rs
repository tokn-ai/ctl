use ctl_client::installation::{resolve_native_updater, select_published_manager};
use ctl_client::remote_bundle::published;
use ctl_core::component::ComponentBuildInfo;
use ctl_core::component_update::BundleChannel;
use ctl_core::installations::{CatalogOrigin, Installations};
use serde::Serialize;
use std::{
  io::{self, Write as _},
  path::Path,
};

const SELECT_CHANNEL_HINT: &str =
  "Run ctlup install updater --channel stable or ctlup install updater --channel nightly.";

#[derive(Serialize)]
struct Status {
  active: Option<String>,
  build: Option<ComponentBuildInfo>,
  origin: Option<CatalogOrigin>,
  tracked_channel: Option<BundleChannel>,
  missing: bool,
}

pub(super) async fn status(home: &Path, json: bool) -> io::Result<()> {
  let store = Installations::new(home);
  let active = if let Some(selection) = store.updater_selection()? {
    Status {
      missing: store.manager_bytes(&selection.content_id)?.is_none(),
      active: Some(selection.content_id),
      build: Some(selection.build),
      origin: selection.origin,
      tracked_channel: selection.tracked_channel,
    }
  } else {
    let (active, build, missing) = match store.manager_and_lease() {
      Ok(Some((path, lease))) => (
        Some(lease.content_id().to_owned()),
        Some(ctl_core::executable::inspect(&path).await?.build),
        false,
      ),
      Ok(None) => (None, None, false),
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        (store.active_manager_id()?, None, true)
      }
      Err(error) => return Err(error),
    };
    Status {
      active,
      build,
      origin: None,
      tracked_channel: None,
      missing,
    }
  };
  if json {
    return super::print_json(&active);
  }
  if active.active.is_none() {
    println!("No managed ctlup is installed. {SELECT_CHANNEL_HINT}");
    return Ok(());
  }
  if let Some(build) = &active.build {
    println!("ctlup {}", build.version);
  }
  let origin = active.origin.as_ref();
  let source = origin.map_or("unknown", |source| source.channel.as_str());
  println!("Build source: {source}");
  if let Some(id) = origin.and_then(|source| source.publication_id.as_deref()) {
    println!("Publication: {id}");
  }
  if source != "stable"
    && let Some(revision) = active.build.and_then(|build| build.source_revision)
  {
    println!("Revision: {revision}");
  }
  match active.tracked_channel {
    Some(channel) => println!("Tracking: {}", channel.name()),
    None => println!("No updater channel selected. {SELECT_CHANNEL_HINT}"),
  }
  if active.missing {
    println!("The selected updater executable is missing.");
  }
  Ok(())
}

pub(super) async fn export(home: &Path, sha256: String) -> io::Result<i32> {
  let store = Installations::new(home);
  tokio::task::spawn_blocking(move || {
    let Some(bytes) = store.manager_bytes(&sha256)? else {
      return Ok(10);
    };
    let mut output = io::stdout().lock();
    output.write_all(&bytes)?;
    output.flush()?;
    Ok(0)
  })
  .await
  .map_err(io::Error::other)?
}

pub(super) async fn install(
  home: &Path,
  requested_channel: Option<BundleChannel>,
  json: bool,
) -> io::Result<()> {
  let expected = Installations::new(home).updater_selection()?;
  let channel = requested_channel
    .or_else(|| {
      expected
        .as_ref()
        .and_then(|selection| selection.tracked_channel)
    })
    .unwrap_or_else(published::default_channel);
  let publication = resolve_native_updater(channel).await?;
  select_published_manager(home, publication, channel, expected).await?;
  status(home, json).await
}

pub(super) async fn update(home: &Path, check: bool, json: bool) -> io::Result<()> {
  let store = Installations::new(home);
  let expected = store.updater_selection()?;
  let Some((selection, channel)) = expected.and_then(|selection| {
    selection
      .tracked_channel
      .map(|channel| (selection, channel))
  }) else {
    return Err(io::Error::other(format!(
      "No updater channel selected. {SELECT_CHANNEL_HINT}"
    )));
  };
  let publication = resolve_native_updater(channel).await?;
  let current = publication.info.sha256 == selection.content_id
    && store.manager_bytes(&selection.content_id)?.is_some();
  if check {
    if json {
      return super::print_json(&serde_json::json!({
        "tracked_channel": channel,
        "installed": selection,
        "available": publication.info,
        "available_origin": publication.origin,
        "current": current,
      }));
    }
    if current {
      println!(
        "ctlup {} is current on {}.",
        selection.build.version,
        channel.name()
      );
    } else {
      println!(
        "ctlup {} is available on {} ({}).",
        publication.info.app_version,
        channel.name(),
        publication.info.bundle_id
      );
    }
    return Ok(());
  }
  if !current || selection.origin.as_ref() != Some(&publication.origin) {
    select_published_manager(home, publication, channel, Some(selection)).await?;
  }
  status(home, json).await
}
