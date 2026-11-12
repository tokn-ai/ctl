use super::*;
use crate::remote_bundle::compatibility::tests::Fixture;
use ctl_core::bundles::{Manifest, Source as BundleSource, Store};
use std::fs;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::PathBuf;

struct Home(PathBuf);
impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctl-active-helper-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    Self(path)
  }

  fn bundled_helper(&self) -> PathBuf {
    let executable = self
      .0
      .join("ctmux.app/Contents/Helpers/ctld.app/Contents/MacOS/ctld");
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    let fixture = Fixture::new("0.1.3", &"a".repeat(40));
    let info = &fixture.inner["components"]["ctld"];
    fs::write(&executable, format!("#!/bin/sh\nprintf '%s' '{info}'\n")).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    executable.canonicalize().unwrap()
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

#[tokio::test]
async fn desktop_checks_the_shared_helper_before_falling_back_to_its_bundle() {
  let home = Home::new();
  let bundled = home.bundled_helper();
  let resolver = Resolver::desktop(None).with_home(&home.0);
  let request = Request::new(Intent::Inspect);
  let selected = resolver
    .select(request, Ok(None), || Some(bundled.clone()))
    .await
    .unwrap();
  assert_eq!(selected.selection().source, Source::DesktopBundle);
  assert_eq!(selected.selection().executable, bundled);

  // A selected shared helper must be checked before even consulting an
  // otherwise usable bundled helper. A damaged selection fails closed.
  let selection =
    ctl_ipc::managed::compatible_selection(&home.0, ctl_core::paths::native_target()).unwrap();
  fs::DirBuilder::new()
    .mode(0o700)
    .recursive(true)
    .create(selection.parent().unwrap())
    .unwrap();
  fs::write(selection, b"not a symlink").unwrap();
  let Err(error) = resolver
    .select(request, Ok(None), || {
      panic!("the bundled fallback was consulted")
    })
    .await
  else {
    panic!("a damaged shared selection must be reported");
  };
  assert!(
    error
      .to_string()
      .contains("must be a symlink owned by this user")
  );
}

#[tokio::test]
async fn only_desktop_can_use_a_bundled_helper_when_the_development_profile_expires() {
  let home = Home::new();
  let bundled = home.bundled_helper();
  let desktop = Resolver::desktop(None).with_home(&home.0);
  let standalone = Resolver::standalone(None, None).with_home(&home.0);
  let expired = || Err(setup::Error::DevelopmentProfileExpired);
  for intent in [Intent::Inspect, Intent::Prepare] {
    let request = Request::new(intent);
    let selected = desktop
      .select(request, expired(), || Some(bundled.clone()))
      .await
      .unwrap();
    assert_eq!(selected.selection().source, Source::DesktopBundle);
    assert!(matches!(
      desktop.select(request, expired(), || None).await,
      Err(setup::Error::DevelopmentProfileExpired)
    ));
    assert!(matches!(
      standalone
        .select(request, expired(), || Some(bundled.clone()))
        .await,
      Err(setup::Error::DevelopmentProfileExpired)
    ));
  }
  assert!(matches!(
    desktop
      .select(Request::new(Intent::Setup), expired(), || Some(
        bundled.clone()
      ))
      .await,
    Err(setup::Error::DevelopmentProfileExpired)
  ));
  assert!(matches!(
    desktop
      .select(
        Request::new(Intent::Prepare),
        Err(setup::Error::Verification("invalid Apple code signature".into())),
        || panic!("the bundled fallback was consulted"),
      )
      .await,
    Err(setup::Error::Verification(message)) if message.contains("invalid Apple code signature")
  ));
}
