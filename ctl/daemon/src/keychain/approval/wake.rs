//! Reauthorize exact selectors used by a previously successful SSH connection.

use super::{Attempt, Error, unavailable};
use ctl_core::observability::{Operation, Outcome};
use ctl_keychain_client::Query;
use std::sync::atomic::AtomicBool;

impl Attempt {
  pub(crate) fn reauthorize(&self, selector: &str, reason: &str) -> Result<(), Error> {
    let (service, account) = selector.split_once(':').ok_or_else(unavailable)?;
    if service == super::super::identity::SERVICE {
      return self.reauthorize_identity(account, reason);
    }
    if !service.starts_with(super::super::SERVICE_PREFIX)
      || account.len() != 64
      || !account.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
      return Err(unavailable());
    }
    let mut history = Operation::credential_read(
      "019b8b45-13a4-7434-9a2a-5960771d8bca",
      account,
      self.history(),
    );
    let result = self.reauthorize_password(selector, service, account, reason, &mut history);
    history.finish(
      if result.is_ok() {
        Outcome::Succeeded
      } else {
        Outcome::Failed
      },
      result.as_ref().err().map(|_| "keychain_operation_failed"),
      result.as_ref().err().and_then(|error| error.os_error()),
    );
    result
  }

  fn reauthorize_password(
    &self,
    selector: &str,
    service: &str,
    account: &str,
    reason: &str,
    history: &mut Operation,
  ) -> Result<(), Error> {
    let guard = super::operation::acquire()?;
    let result = self.read(&guard, selector, reason, |authentication, decision| {
      history.set_authorization_mode(super::super::authorization_mode(decision));
      let records = ctl_keychain_client::search(&Query {
        service: Some(service),
        account: Some(account),
        limit: 1,
        secret: true,
        authentication,
      })?;
      // Native records zeroize secret bytes on drop; no secret leaves this read.
      Ok(
        records
          .first()
          .and_then(|record| record.secret.as_ref())
          .map(|_| ()),
      )
    })?;
    result.ok_or_else(unavailable)
  }

  fn reauthorize_identity(&self, account: &str, reason: &str) -> Result<(), Error> {
    let (identity_id, file_version) = account.split_once(':').ok_or_else(unavailable)?;
    let saved = super::super::index::identity_metadata(identity_id)?.ok_or_else(unavailable)?;
    let snapshot = crate::identities::inspect_path(&saved.path).map_err(|_| unavailable())?;
    if snapshot.identity_id != identity_id || snapshot.file_version != file_version {
      return Err(unavailable());
    }
    // This path checks both the current file and the protected item's binding.
    // It returns a Zeroizing value which is dropped here without decrypting a key.
    let read =
      super::super::identity::load_for_connection(&snapshot, reason, &AtomicBool::new(false), self)
        .map_err(|_| unavailable())?;
    read.map(|_| ()).ok_or_else(unavailable)
  }
}
