use ctl_client::components::inventory::{self, Snapshot};
use std::io;
use std::path::{Path, PathBuf};

pub(super) async fn run(
  home: &Path,
  target: Option<&str>,
  directories: &[PathBuf],
  json: bool,
) -> io::Result<()> {
  let home = home.to_owned();
  let target = target.map(str::to_owned);
  let directories = directories.to_vec();
  let (snapshot, target) = tokio::task::spawn_blocking(move || {
    (
      inventory::snapshot(&home, target.as_deref(), &directories),
      target,
    )
  })
  .await
  .map_err(io::Error::other)?;
  if json {
    println!(
      "{}",
      serde_json::to_string(&json_value(&snapshot, target.as_deref())).map_err(io::Error::other)?
    );
  } else {
    println!("{}", format(&snapshot));
    for error in &snapshot.errors {
      eprintln!(
        "Could not inspect components: {}",
        crate::table::text(error)
      );
    }
  }
  if snapshot.errors.is_empty() {
    Ok(())
  } else {
    Err(io::Error::other(
      "some component builds could not be inspected",
    ))
  }
}

fn json_value(snapshot: &Snapshot, target: Option<&str>) -> serde_json::Value {
  let selected = target.and_then(|target| {
    snapshot
      .selections
      .iter()
      .find(|selection| selection.target_triple == target)
  });
  // Keep manifests and the filtered command's selection fields in their existing shape.
  serde_json::json!({
    "target_triple": target,
    "selected_local": selected.and_then(|selection| selection.selected_local.as_deref()),
    "selected_upload": selected.and_then(|selection| selection.selected_upload.as_deref()),
    "bundles": snapshot.bundles.iter().map(|bundle| &bundle.manifest).collect::<Vec<_>>(),
    "availability": snapshot.bundles.iter().map(|bundle| serde_json::json!({
      "bundle_id": bundle.manifest.bundle_id,
      "target_triple": bundle.manifest.target_triple,
      "included": bundle.availability.included(),
      "stored": bundle.availability.stored(),
      "selected_local": bundle.selected_local,
      "selected_upload": bundle.selected_upload,
    })).collect::<Vec<_>>(),
    "selections": snapshot.selections,
    "errors": snapshot.errors,
  })
}

fn format(snapshot: &Snapshot) -> String {
  if snapshot.bundles.is_empty() {
    if !snapshot.errors.is_empty() {
      return "No complete builds could be verified. See inspection errors.".into();
    }
    return "No complete builds available. Import one with ctl components sync --from <bundle-directory>.".into();
  }
  crate::table::format(
    [
      "TARGET",
      "SOURCE",
      "VERSION",
      "AVAILABLE",
      "SELECTED",
      "BUNDLE",
    ],
    snapshot.bundles.iter().map(|bundle| {
      let manifest = &bundle.manifest;
      let source = match manifest.source {
        ctl_core::bundles::Source::Ci => "ci",
        ctl_core::bundles::Source::Release => "release",
        ctl_core::bundles::Source::Local => "local",
      };
      let labels = |first: bool, second: bool, first_label: &str, second_label: &str| {
        [(first, first_label), (second, second_label)]
          .into_iter()
          .filter_map(|(enabled, label)| enabled.then_some(label))
          .collect::<Vec<_>>()
          .join(", ")
      };
      [
        manifest.target_triple.clone(),
        source.into(),
        manifest.components["ctl-agent"].build.version.clone(),
        labels(
          bundle.availability.included(),
          bundle.availability.stored(),
          "included",
          "stored",
        ),
        labels(
          bundle.selected_local,
          bundle.selected_upload,
          "local",
          "upload",
        ),
        manifest.bundle_id.clone(),
      ]
    }),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn empty_inventory_has_actionable_text_and_machine_readable_output() {
    let snapshot = Snapshot::default();
    assert!(format(&snapshot).starts_with("No complete builds available."));
    assert!(format(&snapshot).contains("ctl components sync --from"));
    let value = json_value(&snapshot, None);
    assert_eq!(value["bundles"], serde_json::json!([]));
    assert_eq!(value["errors"], serde_json::json!([]));
    assert!(value["target_triple"].is_null());
  }
}
