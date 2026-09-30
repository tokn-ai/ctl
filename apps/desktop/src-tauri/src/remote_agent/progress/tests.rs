use super::*;

fn upload(bytes: u64) -> Progress {
  Progress {
    phase: Phase::Transferring,
    file_name: Some("bundle.tar.gz".into()),
    transferred_bytes: bytes,
    total_bytes: 128 * 1024 * 1024,
    bytes_per_second: 0,
  }
}

#[test]
fn slow_healthy_uploads_continue_for_an_hour_without_a_total_deadline() {
  let start = Instant::now();
  let mut watchdog = InstallWatchdog::new(start);
  watchdog.observe(upload(0), start, false).unwrap();
  for second in (10..=3600).step_by(10) {
    let progress = watchdog
      .observe(
        upload(second * 512),
        start + Duration::from_secs(second),
        false,
      )
      .unwrap();
    assert_eq!(progress.bytes_per_second, 512);
  }
}

#[test]
fn stalls_depend_on_recent_speed_and_unchanged_reports_do_not_extend_them() {
  let start = Instant::now();
  let mut fast = InstallWatchdog::new(start);
  let mut slow = InstallWatchdog::new(start);
  for watchdog in [&mut fast, &mut slow] {
    watchdog.observe(upload(0), start, false).unwrap();
  }
  fast
    .observe(upload(1024 * 1024), start + Duration::from_secs(1), false)
    .unwrap();
  slow
    .observe(upload(1024), start + Duration::from_secs(1), false)
    .unwrap();
  assert_eq!(fast.idle_budget(), MIN_TRANSFER_IDLE);
  assert_eq!(slow.idle_budget(), Duration::from_secs(256));
  for second in 2..31 {
    fast
      .observe(
        upload(1024 * 1024),
        start + Duration::from_secs(second),
        false,
      )
      .unwrap();
  }
  let failure = fast
    .observe(upload(1024 * 1024), start + Duration::from_secs(31), false)
    .unwrap_err();
  assert_eq!(failure.code, "remote_agent_install_stalled");
  assert!(failure.message.contains("bundle.tar.gz"));
  assert!(failure.message.contains("1048576 B/s"));
  slow
    .observe(upload(1024), start + Duration::from_secs(31), false)
    .unwrap();
  assert!(
    slow
      .observe(upload(1024), start + Duration::from_secs(257), false)
      .is_err()
  );
}

#[test]
fn authentication_waits_do_not_consume_the_phase_budget() {
  let start = Instant::now();
  let mut watchdog = InstallWatchdog::new(start);
  for second in 1..=180 {
    watchdog
      .observe(initial(), start + Duration::from_secs(second), true)
      .unwrap();
  }
  watchdog
    .observe(initial(), start + Duration::from_secs(239), false)
    .unwrap();
  assert!(
    watchdog
      .observe(initial(), start + Duration::from_mins(4), false)
      .is_err()
  );
}

#[test]
fn extraction_gets_its_own_budget_and_speed_falls_when_upload_stalls() {
  let start = Instant::now();
  let mut watchdog = InstallWatchdog::new(start);
  watchdog.observe(upload(0), start, false).unwrap();
  watchdog
    .observe(upload(1024), start + Duration::from_secs(1), false)
    .unwrap();
  let stalled = watchdog
    .observe(upload(1024), start + Duration::from_secs(12), false)
    .unwrap();
  assert_eq!(stalled.bytes_per_second, 0);
  let mut extracting = upload(1024);
  extracting.phase = Phase::Extracting;
  watchdog
    .observe(extracting.clone(), start + Duration::from_secs(100), false)
    .unwrap();
  watchdog
    .observe(extracting.clone(), start + Duration::from_secs(219), false)
    .unwrap();
  let failure = watchdog
    .observe(extracting, start + Duration::from_secs(220), false)
    .unwrap_err();
  assert!(failure.message.contains("archive extraction"));
}
