use super::{Authentication, Error, Presence, Query, Record, Write};
use core_foundation::array::CFArray;
use core_foundation::base::{CFAllocatorRef, CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::date::CFDate;
use core_foundation::dictionary::CFDictionary;
use core_foundation::error::{CFError, CFErrorRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::passwords::AccessControlOptions;
use security_framework_sys::item::{
  kSecAttrAccessControl, kSecAttrAccount, kSecAttrComment, kSecAttrLabel, kSecAttrService,
  kSecClass, kSecClassGenericPassword, kSecMatchLimit, kSecMatchLimitAll, kSecReturnAttributes,
  kSecReturnData, kSecReturnPersistentRef, kSecReturnRef, kSecUseAuthenticationUI,
  kSecUseDataProtectionKeychain, kSecValueData,
};
use security_framework_sys::keychain_item::{
  SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate,
};
use std::collections::HashMap;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use zeroize::Zeroizing;

// Public Security.framework symbols absent from security-framework-sys 2.17.
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
  static kSecUseOperationPrompt: CFStringRef;
  static kSecUseAuthenticationUIFail: CFStringRef;
  static kSecUseAuthenticationUIAllow: CFStringRef;
  static kSecUseAuthenticationContext: CFStringRef;
  // SecTaskRef is an opaque Core Foundation object; CFType owns its lifetime.
  fn SecTaskCreateFromSelf(allocator: CFAllocatorRef) -> CFTypeRef;
  fn SecTaskCopyValueForEntitlement(
    task: CFTypeRef,
    entitlement: CFStringRef,
    error: *mut CFErrorRef,
  ) -> CFTypeRef;
}

#[link(name = "LocalAuthentication", kind = "framework")]
unsafe extern "C" {}

const NOT_FOUND: i32 = -25_300;
const NOT_AVAILABLE: i32 = -25_291;
const INTERACTION_NOT_ALLOWED: i32 = -25_308;
const MISSING_ENTITLEMENT: i32 = -34_018;
const PARAM: i32 = -50;
const MAX_RESULTS: usize = 8193;
type Parameters = Vec<(CFString, CFType)>;

/// An owned macOS authorization context. Contains no application-supplied secret.
///
/// Keychain operations using this context are serialized, including configuration
/// and the entire synchronous Security call. Invalidating it cancels outstanding
/// authentication immediately and permanently prevents further operations.
pub struct AuthenticationContext {
  context: Retained<AnyObject>,
  operations: Mutex<()>,
  invalidated: AtomicBool,
}

// SAFETY: LAContext can be used off the main thread. We serialize all mutable
// configuration and Keychain operations. The only concurrent method is invalidate,
// whose documented purpose is to cancel an outstanding authentication evaluation.
// No Objective-C reference escapes this wrapper or its operation guard.
unsafe impl Send for AuthenticationContext {}
unsafe impl Sync for AuthenticationContext {}

impl AuthenticationContext {
  /// Create an unauthenticated context. Does not inspect Keychain or display UI.
  ///
  /// # Errors
  /// Returns `errSecNotAvailable` if the native context cannot be constructed.
  pub fn new() -> Result<Self, Error> {
    let class = AnyClass::get(c"LAContext").ok_or(Error(NOT_AVAILABLE))?;
    // SAFETY: the explicitly linked framework exports LAContext. NSObject's new
    // returns an owned object; Option handles an allocation failure without panic.
    let context: Option<Retained<AnyObject>> = unsafe { msg_send![class, new] };
    Ok(Self {
      context: context.ok_or(Error(NOT_AVAILABLE))?,
      operations: Mutex::new(()),
      invalidated: AtomicBool::new(false),
    })
  }

  /// Cancel any in-flight authorization and revoke this context permanently.
  pub fn invalidate(&self) {
    if !self.invalidated.swap(true, Ordering::AcqRel) {
      // SAFETY: LAContext.invalidate is explicitly designed to terminate an
      // existing evaluation. Waiting for operations would leave a dialog live.
      unsafe {
        let _: () = msg_send![&*self.context, invalidate];
      }
    }
  }

  #[must_use]
  pub fn is_invalidated(&self) -> bool {
    self.invalidated.load(Ordering::Acquire)
  }

  fn authorize(&self, reason: &str, allow_ui: bool) -> Result<MutexGuard<'_, ()>, Error> {
    validate_reason(reason)?;
    let guard = self.operations.lock().map_err(|_| Error(NOT_AVAILABLE))?;
    if self.is_invalidated() {
      return Err(Error(INTERACTION_NOT_ALLOWED));
    }
    let reason = CFString::new(reason);
    // SAFETY: CFString is toll-free bridged with NSString; LAContext copies the
    // reason. These property mutations occur only under the operation mutex.
    unsafe {
      let _: () = msg_send![&*self.context, setLocalizedReason: reason.as_concrete_TypeRef().cast::<AnyObject>()];
      let _: () = msg_send![&*self.context, setInteractionNotAllowed: Bool::new(!allow_ui)];
    }
    Ok(guard)
  }

  fn as_cf_type(&self) -> CFType {
    // SAFETY: the live LAContext is an Objective-C object accepted by Core
    // Foundation collection callbacks. The Get-rule wrapper retains it until the
    // Security dictionary is released; it does not reinterpret its layout.
    unsafe { CFType::wrap_under_get_rule(Retained::as_ptr(&self.context).cast()) }
  }
}

impl Drop for AuthenticationContext {
  fn drop(&mut self) {
    self.invalidate();
  }
}

fn validate_reason(reason: &str) -> Result<(), Error> {
  if reason.is_empty() || reason.len() > 4096 || reason.chars().any(char::is_control) {
    Err(Error(PARAM))
  } else {
    Ok(())
  }
}

fn authorization(authentication: Authentication<'_>) -> Result<Option<MutexGuard<'_, ()>>, Error> {
  match authentication {
    Authentication::Context {
      reason,
      context,
      allow_ui,
    } => context.authorize(reason, allow_ui).map(Some),
    Authentication::Allow { .. } | Authentication::Forbid => Ok(None),
  }
}

// All callers supply immutable, process-lifetime Security.framework constants.
fn constant(value: CFStringRef) -> CFString {
  // SAFETY: value is a non-null immutable CFString exported by Security.framework.
  unsafe { CFString::wrap_under_get_rule(value) }
}

fn selector(
  service: Option<&str>,
  account: Option<&str>,
  authentication: Authentication<'_>,
) -> Result<Parameters, Error> {
  if service.is_some_and(str::is_empty) || account.is_some_and(str::is_empty) {
    return Err(Error(PARAM));
  }
  // SAFETY: these statics are immutable CFString references exported by Security.
  let mut parameters = unsafe {
    vec![
      (
        constant(kSecClass),
        constant(kSecClassGenericPassword).into_CFType(),
      ),
      (
        constant(kSecUseDataProtectionKeychain),
        CFBoolean::true_value().into_CFType(),
      ),
    ]
  };
  if let Some(service) = service {
    parameters.push((
      unsafe { constant(kSecAttrService) },
      CFString::new(service).into_CFType(),
    ));
  }
  if let Some(account) = account {
    parameters.push((
      unsafe { constant(kSecAttrAccount) },
      CFString::new(account).into_CFType(),
    ));
  }
  // Never let a caller accidentally fall back to macOS's default interactive UI.
  match authentication {
    Authentication::Forbid => parameters.push(unsafe {
      (
        constant(kSecUseAuthenticationUI),
        constant(kSecUseAuthenticationUIFail).into_CFType(),
      )
    }),
    Authentication::Allow { reason } => {
      validate_reason(reason)?;
      parameters.extend(unsafe {
        [
          (
            constant(kSecUseAuthenticationUI),
            constant(kSecUseAuthenticationUIAllow).into_CFType(),
          ),
          (
            constant(kSecUseOperationPrompt),
            CFString::new(reason).into_CFType(),
          ),
        ]
      });
    }
    Authentication::Context {
      reason,
      context,
      allow_ui,
    } => {
      validate_reason(reason)?;
      parameters.push((
        unsafe { constant(kSecUseAuthenticationContext) },
        context.as_cf_type(),
      ));
      // Keep the query's UI policy explicit as well as the context property.
      // This fails closed even when a context's backing authorization service
      // is unavailable or changes state while configuring the native context.
      parameters.push(unsafe {
        (
          constant(kSecUseAuthenticationUI),
          constant(if allow_ui {
            kSecUseAuthenticationUIAllow
          } else {
            kSecUseAuthenticationUIFail
          })
          .into_CFType(),
        )
      });
    }
  }
  Ok(parameters)
}

fn search_parameters(query: &Query<'_>, attributes: bool) -> Result<Parameters, Error> {
  if query.limit == 0
    || query.limit > MAX_RESULTS
    || (query.secret
      && (query.limit != 1
        || query.service.is_none_or(str::is_empty)
        || query.account.is_none_or(str::is_empty)))
  {
    return Err(Error(PARAM));
  }
  let mut parameters = selector(query.service, query.account, query.authentication)?;
  parameters.extend(unsafe {
    [
      (
        constant(kSecMatchLimit),
        CFNumber::from(i64::try_from(query.limit).map_err(|_| Error(PARAM))?).into_CFType(),
      ),
      (
        constant(kSecReturnAttributes),
        CFBoolean::from(attributes).into_CFType(),
      ),
      (
        constant(kSecReturnData),
        CFBoolean::from(query.secret).into_CFType(),
      ),
      (
        constant(kSecReturnRef),
        CFBoolean::false_value().into_CFType(),
      ),
      (
        constant(kSecReturnPersistentRef),
        CFBoolean::false_value().into_CFType(),
      ),
    ]
  });
  Ok(parameters)
}

fn copy(parameters: &Parameters) -> Result<Option<CFType>, Error> {
  let dictionary = CFDictionary::from_CFType_pairs(parameters);
  let mut result = ptr::null();
  // SAFETY: dictionary and output pointer remain live for the synchronous call.
  let status = unsafe { SecItemCopyMatching(dictionary.as_concrete_TypeRef(), &raw mut result) };
  // Security follows the Create rule for any non-null returned object.
  let result = (!result.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(result) });
  if status == NOT_FOUND {
    Ok(None)
  } else if status == 0 {
    Ok(result)
  } else {
    Err(Error(status))
  }
}

/// Read bounded results. Secret reads must target one item.
///
/// # Errors
/// Returns an `OSStatus` if access is denied or the response is malformed.
pub fn search(query: &Query<'_>) -> Result<Vec<Record>, Error> {
  let _authorization = authorization(query.authentication)?;
  let Some(value) = copy(&search_parameters(query, true)?)? else {
    return Ok(Vec::new());
  };
  let records = if let Some(array) = value.downcast::<CFArray>() {
    if usize::try_from(array.len()).map_err(|_| Error(PARAM))? > query.limit {
      return Err(Error(PARAM));
    }
    array
      .iter()
      .map(|value| {
        // SAFETY: an element of the live CFArray is a valid borrowed CFType.
        record(
          &unsafe { CFType::wrap_under_get_rule(*value) },
          query.secret,
        )
      })
      .collect::<Result<Vec<_>, _>>()?
  } else {
    vec![record(&value, query.secret)?]
  };
  if let Authentication::Context { context, .. } = query.authentication
    && context.is_invalidated()
  {
    // Revocation may race a successful Security call. Do not hand its returned
    // secret to the caller after observing revocation; owned buffers zeroize here.
    return Err(Error(INTERACTION_NOT_ALLOWED));
  }
  Ok(records)
}

fn scan_parameters(authentication: Authentication<'_>) -> Result<Parameters, Error> {
  let mut parameters = selector(None, None, authentication)?;
  parameters.extend(unsafe {
    [
      (
        constant(kSecMatchLimit),
        constant(kSecMatchLimitAll).into_CFType(),
      ),
      (
        constant(kSecReturnAttributes),
        CFBoolean::true_value().into_CFType(),
      ),
      (
        constant(kSecReturnData),
        CFBoolean::false_value().into_CFType(),
      ),
      (
        constant(kSecReturnRef),
        CFBoolean::false_value().into_CFType(),
      ),
      (
        constant(kSecReturnPersistentRef),
        CFBoolean::false_value().into_CFType(),
      ),
    ]
  });
  Ok(parameters)
}

/// Discover owned item attributes without retrieving password data or references.
///
/// Ownership filtering precedes the retained-item limit, so metadata sidecars
/// and unrelated items cannot truncate the source inventory. Any limit or access
/// failure rejects the scan rather than returning an apparently complete subset.
///
/// # Errors
/// Returns an `OSStatus` for access failures, or `ATTRIBUTE_SCAN_LIMIT` for too many
/// owned entries. Authentication UI is controlled explicitly by the caller.
pub fn scan_attributes(
  authentication: Authentication<'_>,
  include: impl Fn(&str) -> bool,
) -> Result<Vec<Record>, Error> {
  let _authorization = authorization(authentication)?;
  let Some(value) = copy(&scan_parameters(authentication)?)? else {
    return Ok(Vec::new());
  };
  let mut records = Vec::new();
  if let Some(array) = value.downcast::<CFArray>() {
    for value in array.iter() {
      // SAFETY: the live result array owns each borrowed Core Foundation value.
      append_owned(
        &unsafe { CFType::wrap_under_get_rule(*value) },
        &include,
        &mut records,
      )?;
    }
  } else {
    append_owned(&value, &include, &mut records)?;
  }
  Ok(records)
}

fn append_owned(
  value: &CFType,
  include: &impl Fn(&str) -> bool,
  records: &mut Vec<Record>,
) -> Result<(), Error> {
  let dictionary = value.downcast::<CFDictionary>().ok_or(Error(PARAM))?;
  let Some(service) =
    dictionary_value(&dictionary, "svce").and_then(|value| value.downcast::<CFString>())
  else {
    return Ok(());
  };
  if include(&service.to_string()) {
    if records.len() == super::MAX_ATTRIBUTE_SCAN_ITEMS {
      return Err(Error(super::ATTRIBUTE_SCAN_LIMIT));
    }
    records.push(record(value, false)?);
  }
  Ok(())
}

fn record(value: &CFType, secret: bool) -> Result<Record, Error> {
  let dictionary = value.downcast::<CFDictionary>().ok_or(Error(PARAM))?;
  let mut attributes = HashMap::new();
  // Only these metadata attributes can cross the FFI boundary as strings.
  for key in ["svce", "acct", "icmt", "labl", "desc"] {
    if let Some(value) =
      dictionary_value(&dictionary, key).and_then(|value| value.downcast::<CFString>())
    {
      attributes.insert(key.into(), value.to_string());
    }
  }
  let data = if secret {
    let value = dictionary_value(&dictionary, "v_Data")
      .and_then(|value| value.downcast::<CFData>())
      .ok_or(Error(PARAM))?;
    if value.len() > 128 * 1024 {
      return Err(Error(PARAM));
    }
    Some(Zeroizing::new(value.bytes().to_vec()))
  } else {
    None
  };
  Ok(Record {
    attributes,
    created_at_ms: date_millis(&dictionary, "cdat"),
    updated_at_ms: date_millis(&dictionary, "mdat"),
    secret: data,
  })
}

fn dictionary_value(dictionary: &CFDictionary, key: &str) -> Option<CFType> {
  let key = CFString::new(key);
  let value = dictionary.find(key.as_CFTypeRef())?;
  // SAFETY: value is owned by the live dictionary; the Get wrapper retains it.
  Some(unsafe { CFType::wrap_under_get_rule(*value) })
}

#[allow(clippy::cast_possible_truncation)]
fn date_millis(dictionary: &CFDictionary, key: &str) -> Option<i64> {
  let date = dictionary_value(dictionary, key)?.downcast::<CFDate>()?;
  let millis = (date.abs_time() + 978_307_200.0) * 1000.0;
  // Dates are display metadata; reject implausible/non-finite values.
  (millis.is_finite() && (-62_135_596_800_000.0..=253_402_300_799_999.0).contains(&millis))
    .then_some(millis.round() as i64)
}

/// Check one exact item without displaying authentication UI or returning data.
///
/// # Errors
/// Returns an `OSStatus` for failures other than a missing/protected item.
pub fn exists(service: &str, account: &str) -> Result<Presence, Error> {
  let query = Query {
    service: Some(service),
    account: Some(account),
    limit: 1,
    secret: false,
    authentication: Authentication::Forbid,
  };
  // Attributes distinguish a successful match from not-found without requesting
  // either a password or a usable Keychain reference.
  match copy(&search_parameters(&query, true)?) {
    Ok(None) => Ok(Presence::Missing),
    Ok(Some(_)) => Ok(Presence::Present),
    Err(Error(INTERACTION_NOT_ALLOWED)) => Ok(Presence::Protected),
    Err(error) => Err(error),
  }
}

/// Check the current process's Data Protection Keychain access using one exact
/// metadata item. Does not return passwords, mutate Keychain, or display UI.
///
/// A missing item is allowed only after checking the process's application ID.
/// macOS may give an unsigned process an implicit read-only smart-card group;
/// its not-found result alone would incorrectly suggest that saving is usable.
/// The subsequent Keychain query makes securityd evaluate runtime provisioning,
/// rather than trusting the entitlement claim or an on-disk signature alone.
///
/// # Errors
/// Returns `errSecMissingEntitlement` for a missing/invalid application ID, or
/// an `OSStatus` if runtime entitlement inspection or the metadata query fails.
/// Successful preflight cannot guarantee a later write/authentication succeeds.
pub fn check_availability(service: &str, account: &str) -> Result<(), Error> {
  with_application_identifier(runtime_application_identifier(), || {
    exists(service, account)
  })
}

fn runtime_application_identifier() -> Result<Option<CFType>, Error> {
  // SAFETY: this public API creates the current process's owned SecTask object.
  let task = unsafe { SecTaskCreateFromSelf(ptr::null()) };
  if task.is_null() {
    return Err(Error(NOT_AVAILABLE));
  }
  // SAFETY: the non-null SecTask follows Core Foundation's Create rule.
  let task = unsafe { CFType::wrap_under_create_rule(task) };
  let name = CFString::new("com.apple.application-identifier");
  let mut error = ptr::null_mut();
  // SAFETY: task, entitlement name, and the output pointer remain live. Both
  // returned values follow the Copy rule and are released by their wrappers.
  let value = unsafe {
    SecTaskCopyValueForEntitlement(
      task.as_CFTypeRef(),
      name.as_concrete_TypeRef(),
      &raw mut error,
    )
  };
  let value = (!value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) });
  let error = (!error.is_null()).then(|| unsafe { CFError::wrap_under_create_rule(error) });
  if error.is_some() {
    // SecTask errors can be POSIX errors, so do not reinterpret their codes as
    // Keychain OSStatus values (or mistake an inspection failure for absence).
    Err(Error(NOT_AVAILABLE))
  } else {
    Ok(value)
  }
}

fn with_application_identifier(
  identifier: Result<Option<CFType>, Error>,
  probe: impl FnOnce() -> Result<Presence, Error>,
) -> Result<(), Error> {
  let identifier = identifier?
    .and_then(|value| value.downcast::<CFString>())
    .map(|value| value.to_string());
  if identifier.as_deref().is_none_or(|value| {
    value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control)
  }) {
    return Err(Error(MISSING_ENTITLEMENT));
  }
  match probe()? {
    Presence::Present | Presence::Missing => Ok(()),
    Presence::Protected => Err(Error(INTERACTION_NOT_ALLOWED)),
  }
}

/// Atomically update data, metadata, and the requested access-control policy.
///
/// # Errors
/// Returns an `OSStatus` if the update/add or user authorization fails. A failed
/// update leaves the existing item intact; it never deletes an item to change ACL.
pub fn upsert(write: &Write<'_>) -> Result<(), Error> {
  let _authorization = authorization(write.authentication)?;
  if write.data.len() > 128 * 1024 {
    return Err(Error(PARAM));
  }
  let mut query = selector(
    Some(write.service),
    Some(write.account),
    write.authentication,
  )?;
  let updates = write_parameters(write)?;
  let selector = CFDictionary::from_CFType_pairs(&query);
  let update = CFDictionary::from_CFType_pairs(&updates);
  // SAFETY: both dictionaries own their values for this synchronous operation.
  let status =
    unsafe { SecItemUpdate(selector.as_concrete_TypeRef(), update.as_concrete_TypeRef()) };
  if status != NOT_FOUND {
    return status_result(status);
  }
  query.extend(updates);
  let query = CFDictionary::from_CFType_pairs(&query);
  // SAFETY: dictionary is live; no output is requested.
  status_result(unsafe { SecItemAdd(query.as_concrete_TypeRef(), ptr::null_mut()) })
}

fn write_parameters(write: &Write<'_>) -> Result<Parameters, Error> {
  let control = SecAccessControl::create_with_protection(
    Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
    if write.user_presence {
      AccessControlOptions::USER_PRESENCE.bits()
    } else {
      0
    },
  )
  .map_err(|error| Error(error.code()))?;
  Ok(unsafe {
    vec![
      (
        constant(kSecValueData),
        // The CFData retains this zeroizing allocation without an unmanaged
        // plaintext copy. Its final release erases our write buffer.
        CFData::from_arc(Arc::new(Zeroizing::new(write.data.to_vec()))).into_CFType(),
      ),
      (
        constant(kSecAttrComment),
        CFString::new(write.comment).into_CFType(),
      ),
      (
        constant(kSecAttrLabel),
        CFString::new(write.label).into_CFType(),
      ),
      (constant(kSecAttrAccessControl), control.into_CFType()),
    ]
  })
}

/// Delete exactly one item, or all accounts in an explicitly supplied service.
///
/// # Errors
/// Returns an `OSStatus` when deletion is denied.
pub fn delete(
  service: &str,
  account: Option<&str>,
  authentication: Authentication<'_>,
) -> Result<(), Error> {
  let _authorization = authorization(authentication)?;
  let parameters = selector(Some(service), account, authentication)?;
  let dictionary = CFDictionary::from_CFType_pairs(&parameters);
  // SAFETY: dictionary is live for the synchronous call.
  let status = unsafe { SecItemDelete(dictionary.as_concrete_TypeRef()) };
  if status == NOT_FOUND {
    Ok(())
  } else {
    status_result(status)
  }
}

fn status_result(status: i32) -> Result<(), Error> {
  if status == 0 {
    Ok(())
  } else {
    Err(Error(status))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn application_identifier() -> CFType {
    CFString::new("FIXTURE123.dev.tokn-ai.ctl.ctld").into_CFType()
  }

  #[test]
  fn missing_or_malformed_application_id_never_probes_keychain() {
    for value in [
      None,
      Some(CFBoolean::true_value().into_CFType()),
      Some(CFString::new("").into_CFType()),
      Some(CFString::new("fixture\napp").into_CFType()),
      Some(CFString::new(&"x".repeat(1025)).into_CFType()),
    ] {
      assert_eq!(
        with_application_identifier(Ok(value), || {
          panic!("unsigned or malformed process must not rely on item-not-found")
        }),
        Err(Error(MISSING_ENTITLEMENT))
      );
    }
    assert_eq!(
      with_application_identifier(Err(Error(NOT_AVAILABLE)), || {
        panic!("failed runtime inspection must not probe Keychain")
      }),
      Err(Error(NOT_AVAILABLE))
    );
  }

  #[test]
  fn authorized_missing_metadata_is_available_but_access_errors_are_preserved() {
    for presence in [Presence::Missing, Presence::Present] {
      assert_eq!(
        with_application_identifier(Ok(Some(application_identifier())), || Ok(presence)),
        Ok(())
      );
    }
    assert_eq!(
      with_application_identifier(Ok(Some(application_identifier())), || Ok(
        Presence::Protected
      )),
      Err(Error(INTERACTION_NOT_ALLOWED))
    );
    for code in [
      MISSING_ENTITLEMENT,
      NOT_AVAILABLE,
      -25_315,
      -25_293,
      -128,
      PARAM,
    ] {
      assert_eq!(
        with_application_identifier(Ok(Some(application_identifier())), || Err(Error(code))),
        Err(Error(code))
      );
    }
  }

  #[test]
  fn secret_reads_require_an_exact_service_and_account() {
    for (service, account, limit) in [
      (None, Some("fixture"), 1),
      (Some("fixture"), None, 1),
      (Some("fixture"), Some("fixture"), 2),
    ] {
      let query = Query {
        service,
        account,
        limit,
        secret: true,
        authentication: Authentication::Allow {
          reason: "Read synthetic credential",
        },
      };
      assert!(search_parameters(&query, true).is_err());
    }
  }

  #[test]
  fn inventory_query_forbids_ui_and_never_requests_secret_or_references() {
    let query = Query {
      service: Some("synthetic-index"),
      account: None,
      limit: 513,
      secret: false,
      authentication: Authentication::Forbid,
    };
    let parameters =
      CFDictionary::from_CFType_pairs(&search_parameters(&query, true).unwrap()).into_untyped();
    assert_eq!(
      dictionary_value(&parameters, "u_AuthUI")
        .unwrap()
        .downcast::<CFString>()
        .unwrap()
        .to_string(),
      "u_AuthUIF"
    );
    for key in ["r_Data", "r_Ref", "r_PersistentRef"] {
      assert!(!bool::from(
        dictionary_value(&parameters, key)
          .unwrap()
          .downcast::<CFBoolean>()
          .unwrap()
      ));
    }
  }

  #[test]
  fn interactive_operations_require_an_explicit_safe_reason() {
    assert!(
      selector(
        Some("fixture"),
        Some("account"),
        Authentication::Allow { reason: "" }
      )
      .is_err()
    );
    assert!(
      selector(
        Some("fixture"),
        Some("account"),
        Authentication::Allow {
          reason: "Read\nsecret"
        }
      )
      .is_err()
    );
    let parameters = CFDictionary::from_CFType_pairs(
      &selector(
        Some("fixture"),
        Some("account"),
        Authentication::Allow {
          reason: "Read SSH password for alice@example.invalid",
        },
      )
      .unwrap(),
    )
    .into_untyped();
    assert_eq!(
      dictionary_value(&parameters, "u_OpPrompt")
        .unwrap()
        .downcast::<CFString>()
        .unwrap()
        .to_string(),
      "Read SSH password for alice@example.invalid"
    );
  }

  #[test]
  fn reusable_context_is_attached_without_implicit_authentication_ui() {
    let context = AuthenticationContext::new().unwrap();
    let authentication = Authentication::Context {
      reason: "Read synthetic credential",
      context: &context,
      allow_ui: false,
    };
    let _authorization = authorization(authentication).unwrap();
    let parameters = CFDictionary::from_CFType_pairs(
      &selector(Some("fixture"), Some("account"), authentication).unwrap(),
    )
    .into_untyped();
    let value = dictionary_value(&parameters, "u_AuthCtx").unwrap();
    assert_eq!(
      value.as_CFTypeRef(),
      Retained::as_ptr(&context.context).cast()
    );
    assert_eq!(
      dictionary_value(&parameters, "u_AuthUI")
        .unwrap()
        .downcast::<CFString>()
        .unwrap()
        .to_string(),
      "u_AuthUIF",
    );
    assert!(dictionary_value(&parameters, "u_OpPrompt").is_none());
  }

  #[test]
  fn invalidation_cancels_without_waiting_for_the_operation_guard() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AuthenticationContext>();
    let context = AuthenticationContext::new().unwrap();
    let guard = context
      .authorize("Read synthetic credential", true)
      .unwrap();
    context.invalidate();
    context.invalidate();
    assert!(context.is_invalidated());
    drop(guard);
    assert!(matches!(
      context.authorize("Read synthetic credential", true),
      Err(Error(INTERACTION_NOT_ALLOWED))
    ));
  }

  #[test]
  fn updating_a_secret_includes_the_requested_access_control() {
    let write = Write {
      service: "fixture",
      account: "account",
      label: "Synthetic credential",
      comment: "Synthetic metadata",
      data: b"synthetic secret",
      user_presence: true,
      authentication: Authentication::Forbid,
    };
    let parameters =
      CFDictionary::from_CFType_pairs(&write_parameters(&write).unwrap()).into_untyped();
    let actual = dictionary_value(&parameters, "accc").unwrap();
    let expected = SecAccessControl::create_with_protection(
      Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
      AccessControlOptions::USER_PRESENCE.bits(),
    )
    .unwrap()
    .into_CFType();
    // Compare native policy objects, including protection and constraints, rather
    // than just checking that the application selected a particular flag.
    assert_eq!(actual, expected);
    assert!(dictionary_value(&parameters, "u_AuthCtx").is_none());
    assert!(dictionary_value(&parameters, "v_Data").is_some());
  }

  #[test]
  #[ignore = "manual macOS test: requires an entitled signed binary and authentication of a synthetic item"]
  fn synthetic_item_upgrades_acl_and_reuses_authorization_without_retaining_secret() {
    const SERVICE: &str = "dev.tokn-ai.ctl.keychain-authorization-test";
    struct Cleanup<'a>(&'a str);
    impl Drop for Cleanup<'_> {
      fn drop(&mut self) {
        let _ = delete(SERVICE, Some(self.0), Authentication::Forbid);
      }
    }
    let account = format!(
      "synthetic-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos(),
    );
    let mut write = Write {
      service: SERVICE,
      account: &account,
      label: "ctl synthetic authorization test",
      comment: "Synthetic test item; contains no user credential",
      data: b"synthetic secret",
      user_presence: false,
      authentication: Authentication::Allow {
        reason: "Update a synthetic test item",
      },
    };
    upsert(&write).unwrap();
    let _cleanup = Cleanup(&account);
    // Exercise SecItemUpdate's policy replacement, not merely the add path.
    write.user_presence = true;
    upsert(&write).unwrap();
    let context = AuthenticationContext::new().unwrap();
    let query = |authentication| Query {
      service: Some(SERVICE),
      account: Some(account.as_str()),
      limit: 1,
      secret: true,
      authentication,
    };
    assert!(matches!(
      search(&query(Authentication::Forbid)),
      Err(Error(INTERACTION_NOT_ALLOWED))
    ));
    for allow_ui in [true, false] {
      // The first query prompts; the second forbids UI and must use OS context
      // authorization. Each returned buffer is zeroized before the next read.
      let records = search(&query(Authentication::Context {
        reason: "Read a synthetic test item",
        context: &context,
        allow_ui,
      }))
      .unwrap();
      assert_eq!(records.len(), 1);
      assert_eq!(
        records[0].secret.as_ref().unwrap().as_slice(),
        b"synthetic secret"
      );
      drop(records);
    }
    context.invalidate();
    assert!(matches!(
      search(&query(Authentication::Context {
        reason: "Read a synthetic test item",
        context: &context,
        allow_ui: false,
      })),
      Err(Error(INTERACTION_NOT_ALLOWED))
    ));
    let fresh_context = AuthenticationContext::new().unwrap();
    assert!(matches!(
      search(&query(Authentication::Context {
        reason: "Read a synthetic test item",
        context: &fresh_context,
        allow_ui: false,
      })),
      Err(Error(INTERACTION_NOT_ALLOWED))
    ));
  }

  #[test]
  fn metadata_projection_does_not_copy_secret_data() {
    let pairs = vec![
      (
        CFString::new("acct"),
        CFString::new("fixture").into_CFType(),
      ),
      (
        CFString::new("v_Data"),
        CFData::from_buffer(b"synthetic secret").into_CFType(),
      ),
      (CFString::new("cdat"), CFDate::new(0.125).into_CFType()),
    ];
    let dictionary = CFDictionary::from_CFType_pairs(&pairs).into_CFType();
    let metadata = record(&dictionary, false).unwrap();
    assert!(metadata.secret.is_none());
    assert_eq!(metadata.created_at_ms, Some(978_307_200_125));
    assert!(!metadata.attributes.contains_key("v_Data"));
    assert_eq!(
      record(&dictionary, true)
        .unwrap()
        .secret
        .unwrap()
        .as_slice(),
      b"synthetic secret"
    );
  }

  #[test]
  fn discovery_requests_all_attributes_with_explicit_authentication_and_no_secrets() {
    let parameters = CFDictionary::from_CFType_pairs(
      &scan_parameters(Authentication::Allow {
        reason: "List synthetic saved credentials",
      })
      .unwrap(),
    )
    .into_untyped();
    assert_eq!(
      dictionary_value(&parameters, "m_Limit")
        .unwrap()
        .downcast::<CFString>()
        .unwrap()
        .to_string(),
      "m_LimitAll",
    );
    assert_eq!(
      dictionary_value(&parameters, "u_AuthUI")
        .unwrap()
        .downcast::<CFString>()
        .unwrap()
        .to_string(),
      "u_AuthUIA",
    );
    assert!(bool::from(
      dictionary_value(&parameters, "r_Attributes")
        .unwrap()
        .downcast::<CFBoolean>()
        .unwrap()
    ));
    for key in ["r_Data", "r_Ref", "r_PersistentRef"] {
      assert!(!bool::from(
        dictionary_value(&parameters, key)
          .unwrap()
          .downcast::<CFBoolean>()
          .unwrap()
      ));
    }
  }

  #[test]
  fn discovery_filters_unrelated_items_before_its_limit_and_never_copies_data() {
    fn item(service: &str) -> CFType {
      CFDictionary::from_CFType_pairs(&[
        (CFString::new("svce"), CFString::new(service).into_CFType()),
        (
          CFString::new("acct"),
          CFString::new("synthetic").into_CFType(),
        ),
        (
          CFString::new("v_Data"),
          CFData::from_buffer(b"fixture secret").into_CFType(),
        ),
      ])
      .into_CFType()
    }
    let unrelated = item("unrelated");
    let owned = item("owned");
    let include = |service: &str| service == "owned";
    let mut records = Vec::new();
    for _ in 0..=super::super::MAX_ATTRIBUTE_SCAN_ITEMS {
      append_owned(&unrelated, &include, &mut records).unwrap();
    }
    assert!(records.is_empty());
    for _ in 0..super::super::MAX_ATTRIBUTE_SCAN_ITEMS {
      append_owned(&owned, &include, &mut records).unwrap();
    }
    assert!(records.iter().all(|record| record.secret.is_none()));
    assert_eq!(
      append_owned(&owned, &include, &mut records),
      Err(Error(super::super::ATTRIBUTE_SCAN_LIMIT))
    );
  }
}
