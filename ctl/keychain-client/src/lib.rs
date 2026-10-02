//! A small owned interface to the macOS Data Protection Keychain.
//!
//! All Core Foundation ownership and Security FFI stays in one audited module.
//! Metadata callers must explicitly forbid authentication; returning attributes
//! alone does not prevent macOS from evaluating an item's biometric ACL.

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod native;

#[cfg(target_os = "macos")]
pub use native::{check_availability, delete, exists, search, upsert};

use std::collections::HashMap;
use zeroize::Zeroizing;

#[derive(Clone, Copy)]
pub enum Authentication<'a> {
  Forbid,
  Allow { reason: &'a str },
}

pub struct Query<'a> {
  pub service: Option<&'a str>,
  pub account: Option<&'a str>,
  pub limit: usize,
  pub secret: bool,
  pub authentication: Authentication<'a>,
}

// Deliberately no Debug: a record may contain a password.
pub struct Record {
  pub attributes: HashMap<String, String>,
  pub created_at_ms: Option<i64>,
  pub updated_at_ms: Option<i64>,
  pub secret: Option<Zeroizing<Vec<u8>>>,
}

pub struct Write<'a> {
  pub service: &'a str,
  pub account: &'a str,
  pub label: &'a str,
  pub comment: &'a str,
  pub data: &'a [u8],
  pub biometric: bool,
  pub authentication: Authentication<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
  Missing,
  Present,
  /// An exact item matched, but its ACL requires authentication.
  Protected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i32);

impl std::fmt::Display for Error {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(formatter, "Keychain operation failed ({})", self.0)
  }
}

impl std::error::Error for Error {}
