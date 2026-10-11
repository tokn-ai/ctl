use super::*;

#[test]
fn unavailable_approval_is_a_policy_failure_without_an_os_status() {
  let error = unavailable();
  assert!(error.is_unavailable());
  assert!(!error.is_locked());
  assert!(!error.is_missing_entitlement());
  assert_eq!(error.os_error(), None);
}

/// Exercise the real retained-approval policy without a Keychain query or
/// authentication evaluation. Native context creation itself cannot display UI.
pub(super) fn failed_retained_attempt() -> Attempt {
  let windows = Arc::new(Windows::<Grant<AuthenticationContext>>::default());
  let now = Instant::now();
  let initial = windows.begin("fixture-host".into(), now);
  let authorized = initial
    .read(
      "fixture-identity",
      true,
      || {
        AuthenticationContext::new()
          .map(|context| Grant::new(Arc::new(context), true))
          .map_err(Error::from)
      },
      || initial.valid(),
      unavailable,
      |context, decision| {
        assert_eq!(decision, ReadDecision::FreshInteractive);
        assert!(!context.native.is_invalidated());
        Ok(Some(()))
      },
    )
    .unwrap();
  assert_eq!(authorized, Some(()));
  initial.connected(now);

  let retained = windows.begin("fixture-host".into(), now);
  let denied: Result<Option<()>, _> = retained.read(
    "fixture-identity",
    true,
    || panic!("retained approval must not create another native context"),
    || retained.valid(),
    unavailable,
    |_, decision| {
      assert_eq!(decision, ReadDecision::RetainedApproval);
      assert!(!decision.allow_ui());
      Err(ctl_keychain_client::Error(-25_308).into())
    },
  );
  let failure = denied.unwrap_err();
  assert!(failure.revoked);
  assert!(failure.error.is_locked());
  assert_eq!(failure.error.os_error(), Some(-25_308));
  assert!(retained.reuse_failed());
  assert!(!retained.valid());

  Attempt {
    pending: Some(retained),
    ..Attempt::fresh(true)
  }
}
