//! Noninteractive access checks, separate from password lookup and save policy.

use super::{Error, index};

/// Checks this process's access to the Data Protection Keychain without reading
/// a password, writing a probe, or showing authentication UI. Check each time:
/// the user can lock Keychain or the daemon can change between connections.
/// Successful preflight cannot guarantee a subsequent interactive operation.
pub(crate) fn availability() -> Result<(), Error> {
  ctl_keychain_client::check_availability(index::SERVICE, index::MARKER).map_err(Into::into)
}

pub(super) fn with_access_policy(
  access: impl FnOnce() -> Result<(), Error>,
  policy: impl FnOnce() -> Result<bool, Error>,
) -> Result<bool, Error> {
  access()?;
  policy()
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::cell::Cell;

  fn error(code: i32) -> Error {
    ctl_keychain_client::Error(code).into()
  }

  #[test]
  fn inaccessible_keychain_never_reaches_save_policy() {
    for code in [
      super::super::MISSING_ENTITLEMENT,
      -25_291,
      -25_308,
      -25_315,
      -25_293,
      -128,
      super::super::operation::BUSY,
      -50,
    ] {
      let result = with_access_policy(
        || Err(error(code)),
        || panic!("unavailable Keychain must not offer credential saving"),
      );
      assert_eq!(result.unwrap_err().0.code(), code);
    }
  }

  #[test]
  fn available_keychain_still_honors_never_save_and_policy_failure() {
    assert!(with_access_policy(|| Ok(()), || Ok(true)).unwrap());
    assert!(!with_access_policy(|| Ok(()), || Ok(false)).unwrap());
    assert_eq!(
      with_access_policy(|| Ok(()), || Err(error(-25_308)))
        .unwrap_err()
        .0
        .code(),
      -25_308
    );
  }

  #[test]
  fn access_is_checked_again_after_a_previous_success() {
    let calls = Cell::new(0);
    let check = || {
      calls.set(calls.get() + 1);
      if calls.get() == 1 {
        Ok(())
      } else {
        Err(error(-25_308))
      }
    };
    assert!(with_access_policy(check, || Ok(true)).unwrap());
    assert_eq!(
      with_access_policy(check, || panic!("locked Keychain must suppress saving"))
        .unwrap_err()
        .0
        .code(),
      -25_308
    );
    assert_eq!(calls.get(), 2);
  }
}
