use std::fs::File;
use std::io::{Cursor, Read as _};
use std::path::Path;
use std::time::SystemTime;

use security_framework::cms::CMSDecoder;

const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

pub(crate) fn has_expired_provisioning_profile() -> bool {
  let Some(executable) = std::env::current_exe().ok() else {
    return false;
  };
  embedded_expiration(&executable).is_some_and(|expiry| expiry <= SystemTime::now())
}

fn embedded_expiration(executable: &Path) -> Option<SystemTime> {
  let contents = executable.parent()?.parent()?;
  if !executable.parent()?.ends_with("Contents/MacOS") {
    return None;
  }
  let mut profile = Vec::new();
  File::open(contents.join("embedded.provisionprofile"))
    .ok()?
    .take(MAX_PROFILE_BYTES + 1)
    .read_to_end(&mut profile)
    .ok()?;
  if profile.len() as u64 > MAX_PROFILE_BYTES {
    return None;
  }
  let decoder = CMSDecoder::create().ok()?;
  decoder.update_message(&profile).ok()?;
  decoder.finalize_message().ok()?;
  let payload = decoder.get_content().ok()?;
  provisioning_expiration(&payload)
}

fn provisioning_expiration(payload: &[u8]) -> Option<SystemTime> {
  let value = plist::Value::from_reader(Cursor::new(payload)).ok()?;
  let date = value.as_dictionary()?.get("ExpirationDate")?.as_date()?;
  Some(SystemTime::from(date))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::{Duration, UNIX_EPOCH};

  #[test]
  fn reads_expiration_as_a_date() {
    let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    for (expiry, expired) in [
      (now - Duration::from_secs(1), true),
      (now, true),
      (now + Duration::from_secs(1), false),
    ] {
      let mut profile = plist::Dictionary::new();
      profile.insert("ExpirationDate".into(), plist::Date::from(expiry).into());
      let mut payload = Vec::new();
      plist::Value::Dictionary(profile)
        .to_writer_xml(&mut payload)
        .unwrap();
      let parsed = provisioning_expiration(&payload);
      assert_eq!(parsed, Some(expiry));
      assert_eq!(parsed.is_some_and(|expiry| expiry <= now), expired);
    }
  }

  #[test]
  fn rejects_missing_or_invalid_expiration() {
    for payload in [
      b"invalid plist".as_slice(),
      b"<plist version=\"1.0\"><dict/></plist>",
      b"<plist version=\"1.0\"><dict><key>ExpirationDate</key><string>invalid</string></dict></plist>",
    ] {
      assert_eq!(provisioning_expiration(payload), None);
    }
  }
}
