//! Local identity-file metadata and verified passphrase operations.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const MAX_REQUEST_BYTES: usize = 128 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_PATHS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
  Ready,
  Missing,
  Unreadable,
  Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassphraseState {
  Saved,
  NotSaved,
  NotRequired,
  FileChanged,
  Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityFile {
  pub identity_id: String,
  pub path: String,
  pub display_path: String,
  pub file_version: Option<String>,
  pub key_type: Option<String>,
  pub fingerprint: Option<String>,
  pub encrypted: Option<bool>,
  pub file_state: FileState,
  pub passphrase_state: PassphraseState,
  pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
  pub identity_files: Vec<IdentityFile>,
  pub complete: bool,
  pub warning: Option<String>,
  pub keychain_available: bool,
}

// Deliberately no Debug: Save contains a secret supplied through a private pipe.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
  List {
    paths: Vec<String>,
  },
  Save {
    path: String,
    file_version: String,
    passphrase: Zeroizing<String>,
  },
  Forget {
    identity_id: String,
  },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
  Inventory { inventory: Inventory },
  Saved,
  Forgotten,
  Error { code: String, message: String },
}
