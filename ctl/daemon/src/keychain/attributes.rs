//! Attribute projection without secret retrieval or raw Core Foundation casts.

use core_foundation::base::TCFType as _;
use core_foundation::dictionary::CFDictionary;
use core_foundation::propertylist::{create_data, kCFPropertyListBinaryFormat_v1_0};
use core_foundation::string::CFString;
use security_framework::item::SearchResult;
use std::io::Cursor;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::credential_metadata::Attributes;

pub(super) fn read(result: &SearchResult) -> Option<Attributes> {
  let SearchResult::Dict(dictionary) = result else {
    return None;
  };
  Some(Attributes {
    values: result.simplify_dict()?,
    created_at_ms: date_millis(dictionary, "cdat"),
    updated_at_ms: date_millis(dictionary, "mdat"),
  })
}

fn date_millis(dictionary: &CFDictionary, key: &str) -> Option<i64> {
  let key = CFString::new(key);
  let value = dictionary.find(key.as_CFTypeRef())?;
  // security-framework returns an untyped dictionary; safely turning a raw
  // value into CFDate is not exposed by core-foundation. Serialize just this
  // live date attribute through its safe property-list API, then require a
  // typed plist date. This neither accesses secret data nor parses debug text.
  let encoded = create_data(*value, kCFPropertyListBinaryFormat_v1_0).ok()?;
  let value = plist::Value::from_reader(Cursor::new(encoded.bytes())).ok()?;
  let timestamp = SystemTime::from(value.as_date()?);
  match timestamp.duration_since(UNIX_EPOCH) {
    Ok(duration) => i64::try_from(duration.as_millis()).ok(),
    Err(error) => i64::try_from(error.duration().as_millis())
      .ok()?
      .checked_neg(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use core_foundation::date::CFDate;

  #[test]
  fn reads_actual_creation_and_modification_dates_including_legacy_items() {
    let dictionary = CFDictionary::from_CFType_pairs(&[
      (
        CFString::new("svce"),
        CFString::new("fixture-service").into_CFType(),
      ),
      (CFString::new("cdat"), CFDate::new(0.125).into_CFType()),
      (CFString::new("mdat"), CFDate::new(60.5).into_CFType()),
    ]);
    let attributes = read(&SearchResult::Dict(dictionary.into_untyped())).unwrap();
    assert_eq!(attributes.created_at_ms, Some(978_307_200_125));
    assert_eq!(attributes.updated_at_ms, Some(978_307_260_500));
  }

  #[test]
  fn absent_or_non_date_attributes_remain_unknown() {
    let dictionary = CFDictionary::from_CFType_pairs(&[(
      CFString::new("cdat"),
      CFString::new("not a date").into_CFType(),
    )]);
    let attributes = read(&SearchResult::Dict(dictionary.into_untyped())).unwrap();
    assert_eq!(attributes.created_at_ms, None);
    assert_eq!(attributes.updated_at_ms, None);
    assert!(read(&SearchResult::Data(Vec::new())).is_none());
  }
}
