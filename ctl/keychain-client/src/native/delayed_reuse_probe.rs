//! Opt-in synthetic-item study. No production callers or credential selectors.

use super::super::SessionState;
use super::{
  Authentication, AuthenticationContext, Error, INTERACTION_NOT_ALLOWED, NOT_AVAILABLE, NOT_FOUND,
  PARAM, Query, Write, constant, copy, delete, exists, kSecAttrAccessControl,
  kSecUseAuthenticationUI, kSecUseAuthenticationUIAllow, kSecUseAuthenticationUIFail, record,
  search_parameters, selector, status_result, write_parameters,
};
use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::passwords::AccessControlOptions;
use security_framework_sys::keychain_item::SecItemAdd;
use serde_json::json;
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod evaluation;
mod responses;

use evaluation::{NativeEvaluation, evaluate_native};
use responses::{exact_one_record, parse_access_control_response};

const SECRET: &[u8] = b"ctl delayed-authorization synthetic item";
const REASON: &str = "Authorize a ctl synthetic Keychain reuse study";
const DEFAULT_OFFSETS: [u64; 2] = [0, 900];

fn offsets() -> Result<Vec<u64>, Error> {
  let Some(value) = std::env::var_os("CTL_KEYCHAIN_REUSE_PROBE_OFFSETS") else {
    return Ok(DEFAULT_OFFSETS.to_vec());
  };
  let value = value.to_str().ok_or(Error(PARAM))?;
  parse_offsets(value)
}

fn parse_offsets(value: &str) -> Result<Vec<u64>, Error> {
  let offsets = value
    .split(',')
    .map(|offset| offset.parse::<u64>().map_err(|_| Error(PARAM)))
    .collect::<Result<Vec<_>, _>>()?;
  if offsets.len() < 2
    || offsets.len() > 16
    || offsets.first() != Some(&0)
    || offsets.last().is_none_or(|offset| *offset > 3600)
    || offsets.windows(2).any(|pair| pair[0] >= pair[1])
  {
    return Err(Error(PARAM));
  }
  Ok(offsets)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protection {
  UserPresence,
  BiometryCurrentSet,
}

impl Protection {
  fn label(self) -> &'static str {
    match self {
      Self::UserPresence => "user_presence",
      Self::BiometryCurrentSet => "biometry_current_set",
    }
  }

  fn from_environment() -> Result<Self, Error> {
    let value = std::env::var_os("CTL_KEYCHAIN_REUSE_PROBE_PROTECTION");
    let value = value
      .as_ref()
      .map(|value| value.to_str().ok_or(Error(PARAM)))
      .transpose()?;
    Self::parse(value)
  }

  fn parse(value: Option<&str>) -> Result<Self, Error> {
    match value {
      None | Some("user_presence") => Ok(Self::UserPresence),
      Some("biometry_current_set") => Ok(Self::BiometryCurrentSet),
      Some(_) => Err(Error(PARAM)),
    }
  }

  fn native_policy(self) -> isize {
    // Public LAPublicDefines.h constants: biometrics=1, device owner=2.
    match self {
      Self::BiometryCurrentSet => 1,
      Self::UserPresence => 2,
    }
  }

  fn native_policy_label(self) -> &'static str {
    match self {
      Self::BiometryCurrentSet => "device_owner_authentication_with_biometrics",
      Self::UserPresence => "device_owner_authentication",
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitialAuthorization {
  KeychainRead,
  KeychainReadAndAclBinding,
  AccessControlEvaluation,
  PolicyEvaluation,
}

impl InitialAuthorization {
  fn label(self) -> &'static str {
    match self {
      Self::KeychainRead => "keychain_read",
      Self::KeychainReadAndAclBinding => "keychain_read_and_acl_binding",
      Self::AccessControlEvaluation => "access_control_evaluation",
      Self::PolicyEvaluation => "policy_evaluation",
    }
  }

  fn from_environment() -> Result<Self, Error> {
    let value = std::env::var_os("CTL_KEYCHAIN_REUSE_PROBE_AUTHORIZATION");
    let value = value
      .as_ref()
      .map(|value| value.to_str().ok_or(Error(PARAM)))
      .transpose()?;
    Self::parse(value)
  }

  fn parse(value: Option<&str>) -> Result<Self, Error> {
    match value {
      None | Some("keychain_read") => Ok(Self::KeychainRead),
      Some("keychain_read_and_acl_binding") => Ok(Self::KeychainReadAndAclBinding),
      Some("access_control_evaluation") => Ok(Self::AccessControlEvaluation),
      Some("policy_evaluation") => Ok(Self::PolicyEvaluation),
      Some(_) => Err(Error(PARAM)),
    }
  }
}

#[derive(Clone, Copy)]
enum UiPolicy {
  DualControls,
  ContextOnly,
}

impl UiPolicy {
  fn label(self) -> &'static str {
    match self {
      Self::DualControls => "dual_controls",
      Self::ContextOnly => "context_property_only",
    }
  }
}

struct Item {
  service: String,
  account: String,
  protection: Protection,
  created: Cell<bool>,
}

impl Item {
  fn new(id: &str, protection: Protection) -> Self {
    Self {
      service: format!("dev.tokn-ai.ctl.delayed-authorization-probe.{id}"),
      account: format!("synthetic-{id}"),
      protection,
      created: Cell::new(false),
    }
  }

  fn create(&self) -> Result<(), Error> {
    super::check_availability(&self.service, &self.account)?;
    // This exact namespace is unique to this experiment. Never upgrade or
    // overwrite an item left by another process, even if identifiers collide.
    if exists(&self.service, &self.account)? != super::Presence::Missing {
      return Err(Error(PARAM));
    }
    let mut parameters = selector(
      Some(&self.service),
      Some(&self.account),
      Authentication::Forbid,
    )?;
    parameters.extend(write_parameters(&Write {
      service: &self.service,
      account: &self.account,
      label: "ctl delayed authorization probe",
      comment: "Synthetic experiment; contains no user credential",
      data: SECRET,
      user_presence: true,
      authentication: Authentication::Forbid,
    })?);
    if self.protection == Protection::BiometryCurrentSet {
      let control = SecAccessControl::create_with_protection(
        Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
        AccessControlOptions::BIOMETRY_CURRENT_SET.bits(),
      )
      .map_err(|error| Error(error.code()))?;
      // SAFETY: the immutable key is exported by Security for this process.
      let access_control_key = unsafe { constant(kSecAttrAccessControl) };
      let (_, access_control) = parameters
        .iter_mut()
        .find(|(key, _)| *key == access_control_key)
        .ok_or(Error(PARAM))?;
      *access_control = control.into_CFType();
    }
    let dictionary = CFDictionary::from_CFType_pairs(&parameters);
    // SAFETY: the dictionary owns all input values for this synchronous call;
    // no output reference is requested. Add-only avoids replacing another item.
    status_result(unsafe { SecItemAdd(dictionary.as_concrete_TypeRef(), std::ptr::null_mut()) })?;
    self.created.set(true);
    Ok(())
  }
}

impl Drop for Item {
  fn drop(&mut self) {
    if !self.created.get() {
      return;
    }
    let status = delete(&self.service, Some(&self.account), Authentication::Forbid)
      .map_or_else(|error| error.0, |()| 0);
    println!(
      "{}",
      json!({
        "event": "cleanup_result",
        "os_status": status,
        "ui_allowed": false,
        "protection": self.protection.label(),
        // Only synthetic selectors are emitted, so a failed cleanup can be
        // repaired with an exact item deletion instead of a broad service wipe.
        "synthetic_service": self.service,
        "synthetic_account": self.account,
      })
    );
  }
}

struct ReadResult {
  stage: &'static str,
  status: Option<i32>,
  native_error: Option<NativeError>,
  succeeded: bool,
}

struct NativeError {
  domain: String,
  code: isize,
}

impl ReadResult {
  fn failure(stage: &'static str, error: Error) -> Self {
    Self {
      stage,
      status: Some(error.0),
      native_error: None,
      succeeded: false,
    }
  }

  fn success() -> Self {
    Self {
      stage: "complete",
      status: Some(0),
      native_error: None,
      succeeded: true,
    }
  }

  fn native_success() -> Self {
    Self {
      status: None,
      ..Self::success()
    }
  }

  fn succeeded(&self) -> bool {
    self.succeeded
  }
}

struct CancellationMonitor {
  done: Arc<AtomicBool>,
  worker: Option<JoinHandle<()>>,
}

impl CancellationMonitor {
  fn start(context: Arc<AuthenticationContext>, path: Option<PathBuf>) -> Self {
    let done = Arc::new(AtomicBool::new(false));
    let worker = path.map(|path| {
      let done = Arc::clone(&done);
      std::thread::spawn(move || {
        while !done.load(Ordering::Acquire) {
          // Only marker existence is read. This thread never touches Keychain
          // and can cancel an outstanding initial UI evaluation immediately.
          if path.try_exists().unwrap_or(true) {
            context.invalidate();
            break;
          }
          std::thread::sleep(Duration::from_millis(100));
        }
      })
    });
    Self { done, worker }
  }
}

impl Drop for CancellationMonitor {
  fn drop(&mut self) {
    self.done.store(true, Ordering::Release);
    if let Some(worker) = self.worker.take() {
      let _ = worker.join();
    }
  }
}

fn read(
  item: &Item,
  context: &AuthenticationContext,
  policy: UiPolicy,
  allow_ui: bool,
) -> ReadResult {
  let _authorization = match context.authorize(REASON, allow_ui) {
    Ok(guard) => guard,
    Err(error) => return ReadResult::failure("native_precheck", error),
  };
  let query = Query {
    service: Some(&item.service),
    account: Some(&item.account),
    limit: 1,
    secret: true,
    authentication: Authentication::Context {
      reason: REASON,
      context,
      allow_ui,
    },
  };
  let mut parameters = match search_parameters(&query, true) {
    Ok(parameters) => parameters,
    Err(error) => return ReadResult::failure("query_configuration", error),
  };
  // Select the experimental policy explicitly, independently of whichever
  // policy the production selector adopts after this study.
  // SAFETY: these are immutable process-lifetime Security CFString constants.
  let ui_key = unsafe { constant(kSecUseAuthenticationUI) };
  parameters.retain(|(key, _)| *key != ui_key);
  if matches!(policy, UiPolicy::DualControls) {
    parameters.push((
      ui_key,
      // SAFETY: both values are immutable Security framework constants.
      unsafe {
        constant(if allow_ui {
          kSecUseAuthenticationUIAllow
        } else {
          kSecUseAuthenticationUIFail
        })
      }
      .into_CFType(),
    ));
  }
  let value = match copy(&parameters) {
    Ok(Some(value)) => value,
    Ok(None) => return ReadResult::failure("keychain_copy", Error(NOT_FOUND)),
    Err(error) => return ReadResult::failure("keychain_copy", error),
  };
  let value = match exact_one_record(value) {
    Ok(value) => value,
    Err(error) => return ReadResult::failure("record_shape", error),
  };
  let decoded = match record(&value, true) {
    Ok(record) => record,
    Err(error) => return ReadResult::failure("record_decode", error),
  };
  let valid = decoded
    .secret
    .as_ref()
    .is_some_and(|data| data.as_slice() == SECRET)
    && decoded.attributes.get("svce") == Some(&item.service)
    && decoded.attributes.get("acct") == Some(&item.account);
  // Clear the owned plaintext before returning even when an assertion fails.
  // Never format Record or compare its buffers with assert_eq! diagnostics.
  drop(decoded);
  drop(value);
  if !valid {
    return ReadResult::failure("record_validation", Error(PARAM));
  }
  if context.is_invalidated() {
    return ReadResult::failure("native_postcheck", Error(INTERACTION_NOT_ALLOWED));
  }
  ReadResult::success()
}

fn exact_access_control(
  item: &Item,
  context: Option<&AuthenticationContext>,
) -> Result<SecAccessControl, Error> {
  let _authorization = context
    .map(|context| context.authorize(REASON, false))
    .transpose()?;
  let authentication = context.map_or(Authentication::Forbid, |context| Authentication::Context {
    reason: REASON,
    context,
    allow_ui: false,
  });
  let parameters = search_parameters(
    &Query {
      service: Some(&item.service),
      account: Some(&item.account),
      limit: 1,
      secret: false,
      authentication,
    },
    true,
  )?;
  parse_access_control_response(copy(&parameters)?.ok_or(Error(NOT_FOUND))?, item)
}

fn evaluate_access_control(item: &Item, context: &AuthenticationContext) -> ReadResult {
  let control = match exact_access_control(item, None) {
    Ok(control) => control,
    Err(error) => return ReadResult::failure("access_control_fetch", error),
  };
  evaluate_native(context, &NativeEvaluation::AccessControl(control), true)
}

struct Observation<'a> {
  session: Arc<SessionState>,
  context: &'a AuthenticationContext,
  context_id: &'a str,
  expected_epoch: u64,
  protection: Protection,
  initial_authorization: InitialAuthorization,
  start: Instant,
}

impl Observation<'_> {
  fn current(&self) -> bool {
    self.session.generation() == self.expected_epoch
      && self.session.is_unlocked()
      && !self.context.is_invalidated()
  }

  fn emit(&self, event: &str, offset: u64, policy: UiPolicy, allow_ui: bool, result: &ReadResult) {
    println!(
      "{}",
      json!({
        "event": event,
        "target_seconds": offset,
        "elapsed_ms": self.start.elapsed().as_millis(),
        "ui_policy": policy.label(),
        "protection": self.protection.label(),
        "initial_authorization": self.initial_authorization.label(),
        "native_policy": matches!(self.initial_authorization, InitialAuthorization::PolicyEvaluation)
          .then(|| self.protection.native_policy_label()),
        "ui_allowed": allow_ui,
        "stage": result.stage,
        "succeeded": result.succeeded(),
        "os_status": result.status,
        "native_error_domain": result.native_error.as_ref().map(|error| &error.domain),
        "native_error_code": result.native_error.as_ref().map(|error| error.code),
        "context_id": self.context_id,
        "context_invalidated": self.context.is_invalidated(),
        "initial_session_epoch": self.expected_epoch,
        "current_session_epoch": self.session.generation(),
        "console_unlocked": self.session.is_unlocked(),
      })
    );
  }
  fn study(&mut self, item: &Item, offsets: &[u64]) -> Result<(), Error> {
    for policy in [UiPolicy::DualControls, UiPolicy::ContextOnly] {
      if !self.current() {
        return Err(Error(INTERACTION_NOT_ALLOWED));
      }
      let baseline = read(item, self.context, policy, false);
      self.emit("fresh_context_baseline", 0, policy, false, &baseline);
      if baseline.status != Some(INTERACTION_NOT_ALLOWED) {
        // A fresh context must prove the ACL actually protects this synthetic
        // item. Other OS errors do not establish the prerequisite either.
        return Err(Error(PARAM));
      }
    }
    if !self.current() {
      return Err(Error(INTERACTION_NOT_ALLOWED));
    }
    let initial = match self.initial_authorization {
      InitialAuthorization::KeychainRead => read(item, self.context, UiPolicy::DualControls, true),
      InitialAuthorization::KeychainReadAndAclBinding => {
        self.authorize_keychain_and_exact_access_control(item)
      }
      InitialAuthorization::AccessControlEvaluation => evaluate_access_control(item, self.context),
      InitialAuthorization::PolicyEvaluation => {
        self.authorize_policy_and_exact_access_control(item)
      }
    };
    self.emit(
      "initial_authorization",
      0,
      UiPolicy::DualControls,
      !matches!(
        self.initial_authorization,
        InitialAuthorization::PolicyEvaluation | InitialAuthorization::KeychainReadAndAclBinding
      ),
      &initial,
    );
    if !initial.succeeded() || !self.current() {
      return Err(Error(if initial.succeeded() {
        INTERACTION_NOT_ALLOWED
      } else {
        initial.status.unwrap_or(NOT_AVAILABLE)
      }));
    }

    self.start = Instant::now();
    self.collect_delayed(item, offsets)
  }

  fn authorize_policy_and_exact_access_control(&self, item: &Item) -> ReadResult {
    let policy = evaluate_native(
      self.context,
      &NativeEvaluation::Policy(self.protection.native_policy()),
      true,
    );
    self.emit(
      "native_policy_authorization",
      0,
      UiPolicy::ContextOnly,
      true,
      &policy,
    );
    if !policy.succeeded() {
      return policy;
    }
    self.bind_exact_access_control(item)
  }

  fn authorize_keychain_and_exact_access_control(&self, item: &Item) -> ReadResult {
    let authorization = read(item, self.context, UiPolicy::DualControls, true);
    self.emit(
      "keychain_authorization",
      0,
      UiPolicy::DualControls,
      true,
      &authorization,
    );
    if !authorization.succeeded() {
      return authorization;
    }
    self.bind_exact_access_control(item)
  }

  fn bind_exact_access_control(&self, item: &Item) -> ReadResult {
    if !self.current() {
      return ReadResult::failure("session_postcheck", Error(INTERACTION_NOT_ALLOWED));
    }
    let control = match exact_access_control(item, Some(self.context)) {
      Ok(control) => control,
      Err(error) => {
        let result = ReadResult::failure("access_control_fetch", error);
        self.emit(
          "exact_access_control_fetch",
          0,
          UiPolicy::DualControls,
          false,
          &result,
        );
        return result;
      }
    };
    self.emit(
      "exact_access_control_fetch",
      0,
      UiPolicy::DualControls,
      false,
      &ReadResult::success(),
    );
    if !self.current() {
      return ReadResult::failure("session_postcheck", Error(INTERACTION_NOT_ALLOWED));
    }
    // Initial authentication is only the prerequisite. Commit the exact
    // persisted ACL's quiet evaluation before permitting the delayed study.
    let result = evaluate_native(
      self.context,
      &NativeEvaluation::AccessControl(control),
      false,
    );
    self.emit(
      "exact_access_control_evaluation",
      0,
      UiPolicy::ContextOnly,
      false,
      &result,
    );
    result
  }

  fn collect_delayed(&self, item: &Item, offsets: &[u64]) -> Result<(), Error> {
    for &offset in offsets {
      let deadline = self.start + Duration::from_secs(offset);
      while Instant::now() < deadline {
        if !self.current() {
          self.emit(
            "session_changed_abort",
            offset,
            UiPolicy::ContextOnly,
            false,
            &ReadResult::failure("session_precheck", Error(INTERACTION_NOT_ALLOWED)),
          );
          return Err(Error(INTERACTION_NOT_ALLOWED));
        }
        std::thread::sleep(
          deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(100)),
        );
      }
      // Preserve the live failure's ordering: a successful supported-policy
      // read before the current-policy read could refresh native authorization.
      for policy in [UiPolicy::DualControls, UiPolicy::ContextOnly] {
        if !self.current() {
          return Err(Error(INTERACTION_NOT_ALLOWED));
        }
        let mut result = read(item, self.context, policy, false);
        if !self.current() {
          result = ReadResult::failure("session_postcheck", Error(INTERACTION_NOT_ALLOWED));
        }
        self.emit("delayed_read", offset, policy, false, &result);
        // An OS refusal is an observation, not a reason to invalidate the
        // shared context or prevent the second policy/timepoint from running.
        if !self.current() {
          return Err(Error(INTERACTION_NOT_ALLOWED));
        }
      }
    }
    Ok(())
  }
}

/// Called only by an explicitly opted-in standalone test entrypoint on the
/// main run loop. One UI operation is permitted; no failed read retries UI.
pub(super) fn run(session: Arc<SessionState>) -> Result<(), Error> {
  // Validate configuration before any Keychain call or mutation.
  let offsets = offsets()?;
  let protection = Protection::from_environment()?;
  let initial_authorization = InitialAuthorization::from_environment()?;
  let cancel_path = std::env::var_os("CTL_KEYCHAIN_REUSE_PROBE_CANCEL_FILE").map(PathBuf::from);
  if cancel_path
    .as_ref()
    .is_some_and(|path| path.try_exists().unwrap_or(true))
  {
    return Err(Error(INTERACTION_NOT_ALLOWED));
  }
  let readiness_deadline = Instant::now() + Duration::from_secs(2);
  while !session.is_unlocked() && Instant::now() < readiness_deadline {
    std::thread::sleep(Duration::from_millis(25));
  }
  if !session.is_unlocked() {
    println!(
      "{}",
      json!({"event": "session_monitor_unavailable_or_locked", "ui_allowed": false})
    );
    return Err(Error(NOT_AVAILABLE));
  }
  let id = format!(
    "{}-{}",
    std::process::id(),
    SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_err(|_| Error(NOT_AVAILABLE))?
      .as_nanos()
  );
  let epoch = session.generation();
  let item = Item::new(&id, protection);
  item.create()?;
  let context = Arc::new(AuthenticationContext::new()?);
  let revoke_context = Arc::clone(&context);
  let _subscription = session.subscribe(Arc::new(move || revoke_context.invalidate()));
  let _cancellation = CancellationMonitor::start(Arc::clone(&context), cancel_path);
  let mut observation = Observation {
    session,
    context: &context,
    context_id: &id,
    expected_epoch: epoch,
    protection,
    initial_authorization,
    start: Instant::now(),
  };
  println!(
    "{}",
    json!({
      "event": "probe_started",
      "context_id": id,
      "session_epoch": epoch,
      "protection": protection.label(),
      "initial_authorization": initial_authorization.label(),
      "scheduled_seconds": offsets,
      "comparison": "same_item_and_context_dual_controls_first",
      "limitation": "successful_reads_may_refresh_os_authorization_reuse_and_affect_second_policy",
      "synthetic_service": item.service,
      "synthetic_account": item.account,
    })
  );
  observation.study(&item, &offsets)?;
  println!(
    "{}",
    json!({"event": "probe_completed", "context_id": id, "protection": protection.label()})
  );
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::{DEFAULT_OFFSETS, InitialAuthorization, Protection, parse_offsets};

  #[test]
  fn initial_authorization_is_explicit_and_never_enables_a_fallback() {
    assert_eq!(
      InitialAuthorization::parse(None).unwrap(),
      InitialAuthorization::KeychainRead
    );
    assert_eq!(
      InitialAuthorization::parse(Some("keychain_read")).unwrap(),
      InitialAuthorization::KeychainRead
    );
    assert_eq!(
      InitialAuthorization::parse(Some("keychain_read_and_acl_binding")).unwrap(),
      InitialAuthorization::KeychainReadAndAclBinding
    );
    assert_eq!(
      InitialAuthorization::parse(Some("access_control_evaluation")).unwrap(),
      InitialAuthorization::AccessControlEvaluation
    );
    assert_eq!(
      InitialAuthorization::parse(Some("policy_evaluation")).unwrap(),
      InitialAuthorization::PolicyEvaluation
    );
    for invalid in [
      "",
      "evaluate_policy",
      "accessControlEvaluation",
      "access_control_evaluation ",
      "both",
    ] {
      assert!(InitialAuthorization::parse(Some(invalid)).is_err());
    }
  }

  #[test]
  fn protection_selection_is_explicit_and_defaults_to_current_saves() {
    assert_eq!(Protection::parse(None).unwrap(), Protection::UserPresence);
    assert_eq!(
      Protection::parse(Some("user_presence")).unwrap(),
      Protection::UserPresence
    );
    assert_eq!(
      Protection::parse(Some("biometry_current_set")).unwrap(),
      Protection::BiometryCurrentSet
    );
    for invalid in [
      "",
      "userPresence",
      "biometry_any",
      "biometry_current_set ",
      "none",
    ] {
      assert!(Protection::parse(Some(invalid)).is_err());
    }
  }

  #[test]
  fn default_schedule_leaves_a_full_fifteen_minute_reuse_gap() {
    assert_eq!(DEFAULT_OFFSETS, [0, 900]);
    assert_eq!(parse_offsets("0,900").unwrap(), DEFAULT_OFFSETS);
    assert_eq!(
      parse_offsets("0,60,300,600,900").unwrap(),
      [0, 60, 300, 600, 900]
    );
  }

  #[test]
  fn invalid_schedules_cannot_trigger_native_setup() {
    for invalid in [
      "", "0", "1,900", "0,0", "0,900,60", "0,-1", "0,3601", "0,abc", "0, 60",
    ] {
      assert!(parse_offsets(invalid).is_err(), "accepted invalid schedule");
    }
    let too_many = (0..17)
      .map(|offset| offset.to_string())
      .collect::<Vec<_>>()
      .join(",");
    assert!(parse_offsets(&too_many).is_err());
  }
}
