use super::{TargetOptions, show};
use std::io;
use std::path::Path;

pub(super) async fn run(home: &Path, targets: &[TargetOptions<'_>], json: bool) -> io::Result<()> {
  if targets.len() == 1 {
    let target = targets[0];
    return show::print(
      &show::snapshot(home, target).await?,
      json,
      !target.remote() && in_checkout(),
    );
  }
  let mut inventories = Vec::new();
  let mut failed = false;
  for &target in targets {
    match show::snapshot(home, target).await {
      Ok(inventory) if json => inventories.push(serde_json::json!({
        "host": target.label(), "inventory": inventory,
      })),
      Ok(inventory) => {
        println!(
          "{}
{}",
          target.label(),
          show::render(&inventory, false)
        );
      }
      Err(error) => {
        failed = true;
        if json {
          inventories.push(serde_json::json!({
            "host": target.label(), "error": error.to_string(),
          }));
        } else {
          eprintln!("ctlup: {}: {error}", target.label());
        }
      }
    }
  }
  if json {
    super::print_json(&inventories)?;
  }
  if failed {
    return Err(io::Error::other("some hosts could not be inspected"));
  }
  Ok(())
}

pub(super) async fn remote_target(options: TargetOptions<'_>) -> io::Result<super::remote::Target> {
  let target = super::remote::Target::connect(options.resolve().await?).await?;
  if target.expected_account().is_none() {
    target.verify_account().await?;
  }
  Ok(target)
}

pub(super) fn in_checkout() -> bool {
  super::development::in_native_checkout()
}
