use super::{TargetOptions, show};
use std::io;
use std::path::Path;

pub(super) async fn run(home: &Path, options: TargetOptions<'_>, json: bool) -> io::Result<()> {
  if !json {
    return show::run(home, options, false).await;
  }
  let status =
    if options.host.is_some() || options.destination.is_some() || options.method.is_some() {
      crate::bundled::register()?;
      let target = remote_target(options).await?;
      ctl_client::installation::remote_status(
        &target.destination,
        &target.options,
        &target.interaction,
        target.expected_account(),
      )
      .await
      .map_err(io::Error::other)?
    } else {
      ctl_client::installation::status(home)?
    };
  println!(
    "{}",
    serde_json::to_string(&status).map_err(io::Error::other)?
  );
  Ok(())
}

pub(super) async fn remote_target(options: TargetOptions<'_>) -> io::Result<super::remote::Target> {
  let mut resolved = crate::target::resolve(options.host, options.destination, options.method)
    .await
    .map_err(io::Error::other)?;
  crate::target::apply_remote_platform(&mut resolved.target, options.platform);
  let target = super::remote::Target::connect(resolved.target).await?;
  if target.expected_account().is_none() {
    target.verify_account().await?;
  }
  Ok(target)
}

pub(super) fn in_checkout() -> bool {
  std::env::current_dir().is_ok_and(|directory| {
    directory.ancestors().any(|root| {
      root.join(".git").exists()
        && root.join("Cargo.toml").is_file()
        && root.join("ctl/cli/Cargo.toml").is_file()
        && root.join("scripts/dev/ctld-signed.mts").is_file()
    })
  })
}
