//! Exact synthetic-item response validation; these helpers perform no Keychain I/O.

use super::{Error, Item, PARAM, constant, kSecAttrAccessControl, record};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use security_framework::access_control::SecAccessControl;

pub(super) fn parse_access_control_response(
  value: CFType,
  item: &Item,
) -> Result<SecAccessControl, Error> {
  let value = exact_one_record(value)?;
  let dictionary = value.downcast::<CFDictionary>().ok_or(Error(PARAM))?;
  if super::super::dictionary_value(&dictionary, "v_Data").is_some() {
    return Err(Error(PARAM));
  }
  let metadata = record(&value, false)?;
  if metadata.attributes.get("svce") != Some(&item.service)
    || metadata.attributes.get("acct") != Some(&item.account)
  {
    return Err(Error(PARAM));
  }
  // SAFETY: the immutable attribute key is exported by Security.framework.
  let key = unsafe { constant(kSecAttrAccessControl) }.to_string();
  // Downcasting retains the persisted item's actual access control. Rebuilding
  // a similarly configured policy would lose its bound native identity.
  super::super::dictionary_value(&dictionary, &key)
    .and_then(|value| value.downcast::<SecAccessControl>())
    .ok_or(Error(PARAM))
}

pub(super) fn exact_one_record(value: CFType) -> Result<CFType, Error> {
  let Some(array) = value.downcast::<CFArray>() else {
    return Ok(value);
  };
  if array.len() != 1 {
    return Err(Error(PARAM));
  }
  let element = array.get(0).ok_or(Error(PARAM))?;
  // SAFETY: the live array owns this borrowed CFType. The Get-rule wrapper
  // retains the element independently before the array/result wrappers drop.
  Ok(unsafe { CFType::wrap_under_get_rule(*element) })
}

#[cfg(test)]
mod tests {
  use super::super::{Item, Protection};
  use super::*;
  use core_foundation::data::CFData;
  use core_foundation::string::CFString;
  use security_framework::access_control::ProtectionMode;
  use security_framework::passwords::AccessControlOptions;

  fn item() -> Item {
    // No Keychain creation: its Drop sees created=false and performs no I/O.
    Item::new("pure-response-fixture", Protection::UserPresence)
  }

  fn fixture(item: &Item, secret: bool) -> (CFType, SecAccessControl) {
    // Creating a policy CF object does not evaluate it or contact Keychain.
    let control = SecAccessControl::create_with_protection(
      Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
      AccessControlOptions::USER_PRESENCE.bits(),
    )
    .unwrap();
    let mut values = vec![
      (
        CFString::new("svce"),
        CFString::new(&item.service).into_CFType(),
      ),
      (
        CFString::new("acct"),
        CFString::new(&item.account).into_CFType(),
      ),
      (CFString::new("accc"), control.clone().into_CFType()),
    ];
    if secret {
      values.push((
        CFString::new("v_Data"),
        CFData::from_buffer(b"synthetic unexpected data").into_CFType(),
      ));
    }
    (
      CFDictionary::from_CFType_pairs(&values).into_CFType(),
      control,
    )
  }

  #[test]
  fn dictionary_response_retains_the_exact_control_after_response_drops() {
    let item = item();
    let (value, expected) = fixture(&item, false);
    let actual = parse_access_control_response(value, &item).unwrap();
    assert!(
      actual == expected,
      "response parsing changed the native ACL"
    );
  }

  #[test]
  fn sole_array_element_retains_the_exact_control_after_array_drops() {
    let item = item();
    let (value, expected) = fixture(&item, false);
    let array = CFArray::from_CFTypes(&[value]).into_CFType();
    let actual = parse_access_control_response(array, &item).unwrap();
    assert!(actual == expected, "array parsing changed the native ACL");
  }

  #[test]
  fn empty_or_multiple_results_never_choose_an_arbitrary_control() {
    let item = item();
    let empty = CFArray::<CFType>::from_CFTypes(&[]).into_CFType();
    assert!(parse_access_control_response(empty, &item).is_err());
    let (value, _) = fixture(&item, false);
    let multiple = CFArray::from_CFTypes(&[value.clone(), value]).into_CFType();
    assert!(parse_access_control_response(multiple, &item).is_err());
  }

  #[test]
  fn secret_data_is_rejected_for_both_metadata_response_shapes() {
    let item = item();
    let (value, _) = fixture(&item, true);
    assert!(parse_access_control_response(value.clone(), &item).is_err());
    let array = CFArray::from_CFTypes(&[value]).into_CFType();
    assert!(parse_access_control_response(array, &item).is_err());
  }

  #[test]
  fn mismatched_selectors_and_non_dictionary_elements_are_rejected() {
    let item = item();
    let another = Item::new("different-selector", Protection::UserPresence);
    let (value, _) = fixture(&another, false);
    assert!(parse_access_control_response(value, &item).is_err());
    let wrong_type = CFString::new("synthetic non-dictionary").into_CFType();
    assert!(parse_access_control_response(wrong_type.clone(), &item).is_err());
    let array = CFArray::from_CFTypes(&[wrong_type]).into_CFType();
    assert!(parse_access_control_response(array, &item).is_err());
  }
}
