use super::*;
use ctl_core::component_update::Package;
use std::cell::RefCell;

fn installed() -> UpdateResult {
  UpdateResult {
    package: Package::FullBundle,
    bundle_id: "fixture".into(),
    target_triple: "aarch64-apple-darwin".into(),
    services_preserved: true,
  }
}

#[tokio::test]
async fn a_failed_host_does_not_discard_successes_or_skip_later_hosts() {
  let (_cancel, mut cancelled) = watch::channel(false);
  let reports = RefCell::new(Vec::new());
  let results = run_batch(
    vec![0, 1, 2],
    &mut cancelled,
    |_, target| async move {
      if target == 1 {
        Err(CommandErrorDto::backend("fixture failure"))
      } else {
        Ok(installed())
      }
    },
    |index, state| {
      reports.borrow_mut().push((index, state));
      Ok(())
    },
  )
  .await
  .unwrap();
  assert_eq!(
    results
      .iter()
      .map(|result| result.state)
      .collect::<Vec<_>>(),
    [State::Complete, State::Failed, State::Complete]
  );
  assert_eq!(results[1].error.as_deref(), Some("fixture failure"));
  assert!(results[0].result.is_some());
  assert!(results[2].result.is_some());
  assert_eq!(
    *reports.borrow(),
    [
      (0, State::Updating),
      (0, State::Complete),
      (1, State::Updating),
      (1, State::Failed),
      (2, State::Updating),
      (2, State::Complete),
    ]
  );
}

#[tokio::test]
async fn stopping_an_active_host_preserves_completed_hosts_and_skips_the_rest() {
  let (cancel, mut cancelled) = watch::channel(false);
  let started = RefCell::new(Vec::new());
  let results = run_batch(
    vec![0, 1, 2],
    &mut cancelled,
    |_, target| {
      started.borrow_mut().push(target);
      let cancel = cancel.clone();
      async move {
        if target == 1 {
          cancel.send_replace(true);
          std::future::pending::<()>().await;
        }
        Ok(installed())
      }
    },
    |_, _| Ok(()),
  )
  .await
  .unwrap();
  assert_eq!(*started.borrow(), [0, 1]);
  assert_eq!(
    results
      .iter()
      .map(|result| result.state)
      .collect::<Vec<_>>(),
    [State::Complete, State::Cancelled, State::Cancelled]
  );
  assert!(results[0].result.is_some());
  assert!(
    results[1]
      .error
      .as_ref()
      .unwrap()
      .contains("activation may already have completed")
  );
}

#[tokio::test]
async fn an_early_stop_never_launches_an_update() {
  let (_cancel, mut cancelled) = watch::channel(true);
  let results = run_batch(
    vec![0, 1],
    &mut cancelled,
    |_, _| async { panic!("a cancelled batch must not install components") },
    |_, _| Ok(()),
  )
  .await
  .unwrap();
  assert!(
    results
      .iter()
      .all(|result| result.state == State::Cancelled)
  );
}

#[test]
fn cancellation_before_registration_is_window_bound_and_expires() {
  let mut registry = Registry::default();
  let key = ("window-a".into(), "attempt".into());
  registry.cancel(key.clone());
  let (cancel, cancelled) = watch::channel(false);
  registry
    .register(("window-b".into(), "attempt".into()), cancel)
    .unwrap();
  assert!(!*cancelled.borrow());
  let (cancel, cancelled) = watch::channel(false);
  registry.register(key.clone(), cancel).unwrap();
  assert!(*cancelled.borrow());
  assert!(registry.pending_cancel.is_empty());
  let expired = ("window-a".into(), "expired".into());
  registry.pending_cancel.insert(
    expired.clone(),
    Instant::now().checked_sub(Duration::from_mins(2)).unwrap(),
  );
  let (cancel, cancelled) = watch::channel(false);
  registry.register(expired, cancel).unwrap();
  assert!(!*cancelled.borrow());
  let (cancel, _) = watch::channel(false);
  assert!(registry.register(key, cancel).is_err());
}

#[test]
fn requests_use_shared_snake_case_models_and_only_two_package_choices() {
  let value = serde_json::json!({
    "targets": [{"kind": "local"}], "attempt_id": "attempt",
    "options": {"package": "ctl_agent", "source": {
      "kind": "provided", "path": "/tmp/build", "local_build": true,
      "ctld_package": "/tmp/ctld.app",
    }},
  });
  let request: UpdateRequest = serde_json::from_value(value.clone()).unwrap();
  assert_eq!(request.options.package, Package::CtlAgent);
  let mut wrong = value.clone();
  wrong["options"]["package"] = serde_json::json!("ctmuxd");
  assert!(serde_json::from_value::<UpdateRequest>(wrong).is_err());
  let mut wrong = value;
  wrong["options"]["source"]["localBuild"] = serde_json::json!(true);
  assert!(serde_json::from_value::<UpdateRequest>(wrong).is_err());
}
