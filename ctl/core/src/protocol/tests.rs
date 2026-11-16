use super::*;

const OLD: ProtocolVersion = ProtocolVersion::new(1, 0, 13);
const NEW: ProtocolVersion = ProtocolVersion::new(1, 1, 15);
const FUTURE: ProtocolVersion = ProtocolVersion::new(1, 2, 16);

#[test]
fn canonical_versions_round_trip_and_order_as_contracts() {
  for version in [OLD, NEW, ProtocolVersion::new(65_535, 65_535, 65_535)] {
    assert_eq!(
      version.to_string().parse::<ProtocolVersion>().unwrap(),
      version
    );
    let bytes = serde_json::to_vec(&version).unwrap();
    assert_eq!(
      serde_json::from_slice::<ProtocolVersion>(&bytes).unwrap(),
      version
    );
  }
  assert_eq!(serde_json::to_string(&OLD).unwrap(), "\"1.0.13\"");
  assert!(OLD < NEW);
  assert!(NEW < ProtocolVersion::new(2, 0, 17));
}

#[test]
fn versions_reject_ranges_prereleases_leading_zeros_and_noncanonical_numbers() {
  for value in [
    "",
    "1",
    "1.0",
    "1.0.13.0",
    "0.0.13",
    "01.0.13",
    "1.00.13",
    "1.0.013",
    "+1.0.13",
    "1.-1.13",
    "1.0.13-beta",
    "1.0.13+build",
    "^1.0.13",
    "1.0.*",
    ">=1.0.13",
    " 1.0.13",
    "1.0.13\n",
    "65536.0.13",
    "1.65536.13",
    "1.0.65536",
  ] {
    assert!(value.parse::<ProtocolVersion>().is_err(), "{value:?}");
    assert!(serde_json::from_value::<ProtocolVersion>(serde_json::json!(value)).is_err());
  }
  for value in [
    serde_json::json!(13),
    serde_json::json!({"major":1,"minor":0,"build":13}),
    serde_json::json!(null),
  ] {
    assert!(serde_json::from_value::<ProtocolVersion>(value).is_err());
  }
  assert!(serde_json::to_string(&ProtocolVersion::new(0, 0, 13)).is_err());
}

#[test]
fn skipped_internal_builds_do_not_become_published_contracts() {
  let internal = ProtocolOffer::new(14, OLD, &[OLD]);
  assert!(internal.is_valid());
  assert!(internal.accepts(OLD));
  assert!(!internal.accepts(ProtocolVersion::new(1, 0, 14)));
  let published = ProtocolOffer::new(15, NEW, &[OLD, NEW]);
  assert!(published.is_valid());
  assert_eq!(published.negotiate(&[OLD]), Some(OLD));
  assert_eq!(published.negotiate(&[ProtocolVersion::new(1, 0, 14)]), None);
}

#[test]
fn newer_and_older_peers_choose_the_same_highest_explicit_intersection() {
  let older = ProtocolOffer::new(13, OLD, &[OLD]);
  let newer = ProtocolOffer::new(15, NEW, &[NEW, OLD]);
  assert_eq!(older.negotiate(&newer.supported_versions), Some(OLD));
  assert_eq!(newer.negotiate(&older.supported_versions), Some(OLD));
  let future = ProtocolOffer::new(16, FUTURE, &[OLD, NEW, FUTURE]);
  assert_eq!(future.negotiate(&newer.supported_versions), Some(NEW));
  assert_eq!(newer.negotiate(&future.supported_versions), Some(NEW));
}

#[test]
fn matching_major_or_version_order_does_not_imply_support() {
  let offer = ProtocolOffer::new(15, NEW, &[OLD, NEW]);
  assert!(!offer.accepts(FUTURE));
  assert_eq!(offer.negotiate(&[FUTURE]), None);
  assert_eq!(offer.negotiate(&[ProtocolVersion::new(2, 0, 17)]), None);
  assert_eq!(offer.negotiate(&[OLD, OLD]), None);
  assert_eq!(
    offer.negotiate(&[OLD, ProtocolVersion::new(2, 0, 17)]),
    None
  );
  assert_eq!(offer.negotiate(&[]), None);
}

#[test]
fn contradictory_or_unbounded_advertisements_are_invalid() {
  for offer in [
    ProtocolOffer::new(12, OLD, &[OLD]),
    ProtocolOffer::new(15, NEW, &[]),
    ProtocolOffer::new(15, NEW, &[OLD]),
    ProtocolOffer::new(15, NEW, &[OLD, OLD, NEW]),
    ProtocolOffer::new(16, NEW, &[OLD, NEW, FUTURE]),
    ProtocolOffer::new(17, NEW, &[OLD, NEW, ProtocolVersion::new(2, 0, 17)]),
    ProtocolOffer::new(15, NEW, &[ProtocolVersion::new(1, 0, 15), NEW]),
    ProtocolOffer::new(16, NEW, &[ProtocolVersion::new(1, 0, 16), NEW]),
    ProtocolOffer::new(15, NEW, &[ProtocolVersion::new(0, 0, 13), NEW]),
  ] {
    assert!(!offer.is_valid(), "{offer:?}");
    assert!(!offer.accepts(OLD));
    assert_eq!(offer.negotiate(&[OLD, NEW]), None);
    assert!(serde_json::to_string(&offer).is_err());
  }
  let versions: Vec<_> = (0..129)
    .map(|build| ProtocolVersion::new(1, 0, build))
    .collect();
  assert!(!ProtocolOffer::new(128, versions[128], &versions).is_valid());
  assert!(ProtocolOffer::new(127, versions[127], &versions[..128]).is_valid());
}

#[test]
fn offer_json_requires_valid_explicit_contract_metadata() {
  let offer = ProtocolOffer::new(15, NEW, &[OLD, NEW]);
  let value = serde_json::to_value(&offer).unwrap();
  assert_eq!(
    value,
    serde_json::json!({"build":15,"version":"1.1.15","supported_versions":["1.0.13","1.1.15"]})
  );
  assert_eq!(
    serde_json::from_value::<ProtocolOffer>(value.clone()).unwrap(),
    offer
  );
  for (field, replacement) in [
    ("build", serde_json::json!(12)),
    ("version", serde_json::json!("01.1.15")),
    ("supported_versions", serde_json::json!([])),
    (
      "supported_versions",
      serde_json::json!(["1.0.13", "1.0.13", "1.1.15"]),
    ),
    (
      "supported_versions",
      serde_json::json!(["1.1.15", "2.0.17"]),
    ),
    ("unexpected", serde_json::json!(true)),
  ] {
    let mut invalid = value.clone();
    invalid[field] = replacement;
    assert!(
      serde_json::from_value::<ProtocolOffer>(invalid).is_err(),
      "{field}"
    );
  }
  assert!(serde_json::from_str::<ProtocolOffer>(r#"{"build":15,"version":"1.1.15"}"#).is_err());
}
