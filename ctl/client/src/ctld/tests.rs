use super::*;
use crate::remote_bundle::compatibility::tests::Fixture;
use ctl_core::bundles::{Manifest, Source as BundleSource, Store};
use std::fs;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;

struct Home(PathBuf);
impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctl-active-helper-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    Self(path)
  }
}
impl Drop for Home {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[tokio::test]
async fn explicit_complete_installation_precedes_checkout_for_cli_and_desktop() {
  let home = Home::new();
  let context = DevelopmentContext {
    repository_root: "/fixture-checkout",
    checkpoints: &["/fixture-checkpoint"],
  };
  for resolver in [
    Resolver::standalone(None, Some(&context)),
    Resolver::desktop(Some(&context)),
  ] {
    let error = resolver
      .with_home(&home.0)
      .resolve(Request::new(Intent::Inspect))
      .await
      .unwrap_err();
    assert!(error.to_string().contains("checkout development helper"));
  }

  let fixture = Fixture::new("0.1.2", &"a".repeat(40)).with_clients();
  let components = serde_json::from_value(fixture.inner["components"].clone()).unwrap();
  let mut files = fixture.files;
  files.insert("ctld.app/Contents/MacOS/ctld".into(), files["ctld"].clone());
  // A deliberately invalid receipt proves that explicit installation errors
  // are surfaced before consulting the stale checkout. No native signing or
  // helper metadata execution is needed to verify this selection boundary.
  files.insert("ctld-package.json".into(), b"{}".to_vec());
  let manifest = Manifest::new(
    ctl_core::paths::native_target(),
    BundleSource::Ci,
    components,
    &files,
  )
  .unwrap();
  let bundle = Store::cache(&home.0).publish(&manifest, &files).unwrap();
  let installations = ctl_core::installations::Installations::new(&home.0);
  installations.activate(&bundle).unwrap();
  for resolver in [
    Resolver::standalone(None, Some(&context)),
    Resolver::desktop(Some(&context)),
  ] {
    let error = resolver
      .with_home(&home.0)
      .resolve(Request::new(Intent::Inspect))
      .await
      .unwrap_err();
    assert!(!error.to_string().contains("checkout development helper"));
    assert!(
      error.to_string().contains("invalid ctld release"),
      "{error}"
    );
  }
  assert_eq!(installations.active().unwrap().unwrap().manifest, manifest);
}
