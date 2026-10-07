use super::*;

fn bundle(mode: BundleMode) -> EmbeddedBundle {
  EmbeddedBundle {
    manifest: &[],
    archive: &[],
    mode,
  }
}

#[tokio::test]
async fn development_uses_its_embedded_helper_without_consulting_shared_selection() {
  let result = select_helper(
    Some(bundle(BundleMode::Development)),
    async { panic!("embedded development must not consult a local checkpoint") },
    async { panic!("development must not consult another checkout's selection") },
    |_| async { Ok(PathBuf::from("matching-development-helper")) },
  )
  .await
  .unwrap();
  assert_eq!(result, Some(PathBuf::from("matching-development-helper")));

  let error = select_helper(
    Some(bundle(BundleMode::Development)),
    async { Ok(Some(PathBuf::from("unrelated-local-helper"))) },
    async { Ok(Some(PathBuf::from("older-shared-helper"))) },
    |_| async {
      Err(setup::Error::Verification(
        "invalid matching signature".into(),
      ))
    },
  )
  .await
  .unwrap_err();
  assert!(matches!(error, setup::Error::Verification(_)));
}

#[tokio::test]
async fn release_reuses_capable_shared_helpers_and_prepares_its_bundle_when_needed() {
  for shared in [None, Some(PathBuf::from("capable-shared-helper"))] {
    let expected = shared
      .clone()
      .unwrap_or_else(|| PathBuf::from("bundled-release-helper"));
    let result = select_helper(
      Some(bundle(BundleMode::Signed)),
      async { panic!("a release payload must not consult debug helper provenance") },
      async { Ok(shared) },
      |_| async { Ok(PathBuf::from("bundled-release-helper")) },
    )
    .await
    .unwrap();
    assert_eq!(result, Some(expected));
  }
  let result = select_helper(
    None,
    async { Ok(None) },
    async { Ok(Some(PathBuf::from("shared-for-cargo-build"))) },
    |_| async { panic!("ordinary Cargo builds contain no bundle") },
  )
  .await
  .unwrap();
  assert_eq!(result, Some(PathBuf::from("shared-for-cargo-build")));
}

#[tokio::test]
async fn invalid_shared_selections_are_not_bypassed_by_release_bundles() {
  let error = select_helper(
    Some(bundle(BundleMode::Signed)),
    async { panic!("a release payload must not consult debug helper provenance") },
    async { Err(setup::Error::Verification("untrusted selection".into())) },
    |_| async { panic!("a release must not hide trust failures") },
  )
  .await
  .unwrap_err();
  assert!(matches!(error, setup::Error::Verification(_)));
}

#[tokio::test]
async fn ordinary_debug_builds_use_local_helpers_before_shared_selections() {
  let local = PathBuf::from("verified-local-development-helper");
  let result = select_helper(
    None,
    async { Ok(Some(local.clone())) },
    async { panic!("a verified local helper must win over shared selection") },
    |_| async { panic!("ordinary Cargo builds contain no bundle") },
  )
  .await
  .unwrap();
  assert_eq!(result, Some(local));
}

#[tokio::test]
async fn absent_or_incompatible_local_helpers_preserve_shared_fallback() {
  let shared = PathBuf::from("verified-shared-helper");
  let result = select_helper(
    None,
    async { Ok(None) },
    async { Ok(Some(shared.clone())) },
    |_| async { panic!("ordinary Cargo builds contain no bundle") },
  )
  .await
  .unwrap();
  assert_eq!(result, Some(shared));
}

#[tokio::test]
async fn invalid_local_checkpoints_do_not_silently_fall_back_to_shared_helpers() {
  let error = select_helper(
    None,
    async { Err(setup::Error::Verification("invalid local signature".into())) },
    async { panic!("trust failures must be reported before shared fallback") },
    |_| async { panic!("ordinary Cargo builds contain no bundle") },
  )
  .await
  .unwrap_err();
  assert!(matches!(error, setup::Error::Verification(_)));
}

#[cfg(all(target_os = "macos", debug_assertions))]
#[tokio::test]
#[ignore = "requires a provisioned checkout-local ctld and CTL_TEST_DEVELOPMENT_CTLD"]
async fn ordinary_cargo_provenance_selects_the_provisioned_helper() {
  if BUNDLED_CTLD.is_some() {
    return;
  }
  assert!(local::enabled());
  let expected = PathBuf::from(
    std::env::var_os("CTL_TEST_DEVELOPMENT_CTLD")
      .expect("set CTL_TEST_DEVELOPMENT_CTLD to the provisioned ctld.app/Contents/MacOS/ctld"),
  )
  .canonicalize()
  .unwrap();
  // These only verify signed helper metadata; they never run a broker, perform
  // Keychain discovery, or update a global component selection.
  for required in [None, Some(ctl_ipc::HELPER_API_VERSION)] {
    assert_eq!(
      prepare_once(required)
        .await
        .unwrap()
        .unwrap()
        .canonicalize()
        .unwrap(),
      expected,
    );
  }
}
