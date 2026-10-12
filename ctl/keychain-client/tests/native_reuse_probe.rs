//! Standalone opt-in runner, so the session monitor owns the real main run loop.

// Recompiling the private wrapper in this test target keeps experimental query
// policy and diagnostics out of the production public API.
#[cfg(target_os = "macos")]
#[path = "../src/lib.rs"]
#[allow(dead_code, unused_imports)]
mod keychain_client;

#[cfg(target_os = "macos")]
fn main() {
  if std::env::var_os("CTL_RUN_NATIVE_REUSE_PROBE").as_deref() != Some(std::ffi::OsStr::new("1")) {
    println!(
      "{}",
      serde_json::json!({"event": "probe_skipped", "reason": "explicit_opt_in_required"})
    );
    return;
  }
  if let Err(error) =
    keychain_client::with_session_monitor(keychain_client::run_delayed_reuse_probe)
  {
    println!(
      "{}",
      serde_json::json!({"event": "probe_aborted", "probe_error_code": error.0})
    );
    std::process::exit(1);
  }
}

#[cfg(not(target_os = "macos"))]
fn main() {
  println!(
    "{}",
    serde_json::json!({"event": "probe_skipped", "reason": "macos_required"})
  );
}
