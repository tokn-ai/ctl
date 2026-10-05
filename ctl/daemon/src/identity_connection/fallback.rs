//! Retain local fallback causes until SSH actually needs a manual key prompt.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::identities::IdentityError;

#[derive(Clone, Copy)]
pub(super) enum Reason {
  NotSaved,
  Identity(IdentityError),
  PublicHintUnavailable,
  AgentUnavailable,
  TimedOut,
  WorkerFailed,
}

impl Reason {
  pub(super) fn warning(self) -> String {
    let reason = match self {
      Self::NotSaved => "No saved passphrase was found for this identity file.".into(),
      Self::Identity(IdentityError::FileChanged) => {
        "The identity file changed. Its saved passphrase cannot be reused automatically.".into()
      }
      Self::Identity(error) => error.to_string(),
      Self::PublicHintUnavailable => {
        "The saved passphrase cannot be used automatically because the identity's public key could not be verified.".into()
      }
      Self::AgentUnavailable => "The local SSH identity agent could not use the saved passphrase.".into(),
      Self::TimedOut => "Reading or verifying the saved passphrase timed out.".into(),
      Self::WorkerFailed => "The saved identity passphrase could not be checked.".into(),
    };
    format!("{reason} Enter the passphrase manually for this connection.")
  }
}

#[derive(Default)]
pub(super) struct Fallbacks(Mutex<HashMap<String, Reason>>);

impl Fallbacks {
  pub(super) fn record(&self, identity_id: &str, reason: Reason) {
    self.0.lock().unwrap().insert(identity_id.into(), reason);
  }

  pub(super) fn clear(&self, identity_id: &str) {
    self.0.lock().unwrap().remove(identity_id);
  }

  pub(super) fn warning(&self, identity_id: &str) -> Option<String> {
    self
      .0
      .lock()
      .unwrap()
      .get(identity_id)
      .copied()
      .map(Reason::warning)
  }
}
