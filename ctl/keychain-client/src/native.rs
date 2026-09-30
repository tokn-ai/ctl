use super::{Authentication, Error, Presence, Query, Record, Write};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::date::CFDate;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::passwords::AccessControlOptions;
use security_framework_sys::item::{
  kSecAttrAccessControl, kSecAttrAccount, kSecAttrComment, kSecAttrLabel, kSecAttrService,
  kSecClass, kSecClassGenericPassword, kSecMatchLimit, kSecReturnAttributes, kSecReturnData,
  kSecReturnPersistentRef, kSecReturnRef, kSecUseAuthenticationUI, kSecUseDataProtectionKeychain,
  kSecValueData,
};
use security_framework_sys::keychain_item::{
  SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate,
};
use std::collections::HashMap;
use std::ptr;
use std::sync::Arc;
use zeroize::Zeroizing;

// Public Security.framework symbols absent from security-framework-sys 2.17.
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
  static kSecUseOperationPrompt: CFStringRef;
  static kSecUseAuthenticationUIFail: CFStringRef;
  static kSecUseAuthenticationUIAllow: CFStringRef;
}

const NOT_FOUND: i32 = -25_300;
const INTERACTION_NOT_ALLOWED: i32 = -25_308;
const PARAM: i32 = -50;
const MAX_RESULTS: usize = 8193;
type Parameters = Vec<(CFString, CFType)>;

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
      if reason.is_empty() || reason.len() > 4096 || reason.chars().any(char::is_control) {
        return Err(Error(PARAM));
      }
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
  let Some(value) = copy(&search_parameters(query, true)?)? else {
    return Ok(Vec::new());
  };
  if let Some(array) = value.downcast::<CFArray>() {
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
      .collect()
  } else {
    Ok(vec![record(&value, query.secret)?])
  }
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

/// Atomically update data and metadata, retaining an existing item's ACL.
///
/// # Errors
/// Returns an `OSStatus` if the update/add or biometric authorization fails.
pub fn upsert(write: &Write<'_>) -> Result<(), Error> {
  if write.data.len() > 128 * 1024 {
    return Err(Error(PARAM));
  }
  let mut query = selector(
    Some(write.service),
    Some(write.account),
    write.authentication,
  )?;
  let updates = unsafe {
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
    ]
  };
  let selector = CFDictionary::from_CFType_pairs(&query);
  let update = CFDictionary::from_CFType_pairs(&updates);
  // SAFETY: both dictionaries own their values for this synchronous operation.
  let status =
    unsafe { SecItemUpdate(selector.as_concrete_TypeRef(), update.as_concrete_TypeRef()) };
  if status != NOT_FOUND {
    return status_result(status);
  }
  let flags = if write.biometric {
    AccessControlOptions::BIOMETRY_CURRENT_SET.bits()
  } else {
    0
  };
  let control = SecAccessControl::create_with_protection(
    Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
    flags,
  )
  .map_err(|error| Error(error.code()))?;
  query.push((
    unsafe { constant(kSecAttrAccessControl) },
    control.into_CFType(),
  ));
  query.extend(updates);
  let query = CFDictionary::from_CFType_pairs(&query);
  // SAFETY: dictionary is live; no output is requested.
  status_result(unsafe { SecItemAdd(query.as_concrete_TypeRef(), ptr::null_mut()) })
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
}
