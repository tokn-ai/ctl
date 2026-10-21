use std::io;

pub(super) async fn run(channel: Option<super::Channel>, json: bool) -> io::Result<()> {
  let channel = channel.map_or_else(
    ctl_client::remote_bundle::published::default_channel,
    Into::into,
  );
  let catalog = ctl_client::remote_bundle::published::check(channel)
    .await
    .map_err(io::Error::other)?;
  if json {
    println!(
      "{}",
      serde_json::to_string(&catalog).map_err(io::Error::other)?
    );
  } else {
    println!(
      "{} channel: {} ({}, revision {})",
      catalog.channel.name(),
      catalog.bundle_id,
      catalog.app_version,
      catalog.git_revision
    );
    for target in catalog.targets {
      let ctmux = target
        .components
        .get("ctmuxd")
        .and_then(|info| {
          info
            .protocols
            .iter()
            .find(|protocol| protocol.name == "ctmux")
        })
        .map_or_else(|| "unknown".into(), |protocol| protocol.version.to_string());
      println!(
        "{}: {} · ctmux {}",
        target.target_triple,
        if target.compatible {
          "compatible"
        } else {
          "incompatible"
        },
        ctmux
      );
    }
    println!("No components were downloaded or installed; running services were unchanged.");
  }
  Ok(())
}
