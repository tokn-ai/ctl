//! One-shot, metadata-only access through ctld's signed Keychain identity.

use ctl_ipc::credentials::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Request, Response};
use std::io::{self, Read, Write};

/// Read one request and write one JSON response, without starting a daemon.
///
/// # Errors
/// Returns an error only when process input/output cannot be read or written.
pub fn run(reader: impl Read, writer: impl Write) -> io::Result<()> {
  run_with(reader, writer, handle)
}

fn run_with(
  reader: impl Read,
  mut writer: impl Write,
  handle: impl FnOnce(&Request) -> Response,
) -> io::Result<()> {
  let mut bytes = Vec::new();
  reader
    .take((MAX_REQUEST_BYTES + 1) as u64)
    .read_to_end(&mut bytes)?;
  let request = if bytes.len() > MAX_REQUEST_BYTES {
    None
  } else {
    serde_json::from_slice::<Request>(&bytes).ok()
  };
  let response = request.as_ref().map_or_else(
    || {
      error(
        "credential_request_invalid",
        "The credential request is invalid.",
      )
    },
    handle,
  );
  let mut bytes = encode_response(response)?;
  bytes.push(b'\n');
  writer.write_all(&bytes)?;
  writer.flush()
}

fn encode_response(mut response: Response) -> io::Result<Vec<u8>> {
  loop {
    let encoded = serde_json::to_vec(&response).map_err(io::Error::other)?;
    if encoded.len() < MAX_RESPONSE_BYTES {
      return Ok(encoded);
    }
    match &mut response {
      Response::Inventory { inventory } if !inventory.credentials.is_empty() => {
        // A bounded search already caps item count. Halving limits repeated
        // serialization to logarithmic work when names fill the byte budget.
        inventory
          .credentials
          .truncate(inventory.credentials.len() / 2);
        inventory.complete = false;
        inventory.warning = Some(crate::credential_metadata::TRUNCATED_WARNING.into());
      }
      Response::Discovered { .. } => {
        response = error(
          "credential_discovery_limit",
          "The saved credential inventory exceeds the helper response size limit.",
        );
      }
      _ => {
        return Err(io::Error::other(
          "credential helper response exceeds its size limit",
        ));
      }
    }
  }
}

fn error(code: &str, message: &str) -> Response {
  Response::Error {
    code: code.into(),
    message: message.into(),
  }
}

fn handle(request: &Request) -> Response {
  use ctl_core::observability::{Event, Operation, Outcome};
  let (event, subject) = match request {
    Request::Forget { credential_id } => (Event::CredentialRemove, Some(credential_id.as_str())),
    Request::Clear {} => (Event::CredentialClear, None),
    _ => (Event::CredentialInventory, None),
  };
  let operation = Operation::start(event, subject);
  let response = handle_inner(request);
  operation.finish(
    if matches!(response, Response::Error { .. }) {
      Outcome::Failed
    } else {
      Outcome::Succeeded
    },
    match &response {
      Response::Error { code, .. } => Some(audit_error_code(code)),
      _ => None,
    },
    None,
  );
  response
}

// Never persist arbitrary error strings, even when they came from a helper.
fn audit_error_code(code: &str) -> &'static str {
  match code {
    "credential_request_invalid" => "credential_request_invalid",
    "credential_list_failed" => "credential_list_failed",
    "credential_import_failed" => "credential_import_failed",
    "credential_discovery_failed" => "credential_discovery_failed",
    "credential_forget_failed" => "credential_forget_failed",
    "credential_clear_failed" => "credential_clear_failed",
    "credential_store_unsupported" => "credential_store_unsupported",
    "credential_store_busy" => "credential_store_busy",
    "credential_store_missing_entitlement" => "credential_store_missing_entitlement",
    "credential_store_unavailable" => "credential_store_unavailable",
    "credential_store_locked" => "credential_store_locked",
    "credential_discovery_limit" => "credential_discovery_limit",
    "credential_discovery_conflict" => "credential_discovery_conflict",
    _ => "credential_request_failed",
  }
}

fn handle_inner(request: &Request) -> Response {
  if let Request::Forget { credential_id } = request
    && crate::credential_metadata::item_identity(credential_id).is_none()
  {
    return error(
      "credential_request_invalid",
      "The credential identifier is invalid.",
    );
  }
  #[cfg(target_os = "macos")]
  {
    match request {
      Request::List | Request::ListMetadata => match crate::keychain::list() {
        Ok(inventory) => Response::Inventory { inventory },
        Err(failure) => keychain_error(failure, "credential_list_failed"),
      },
      Request::ImportMetadata => match crate::keychain::import_metadata() {
        Ok(()) => Response::Imported,
        Err(failure) => keychain_error(failure, "credential_import_failed"),
      },
      Request::Discover {} => match crate::keychain::discover() {
        Ok(inventory) => Response::Discovered { inventory },
        Err(failure) => keychain_error(failure, "credential_discovery_failed"),
      },
      Request::Forget { credential_id } => match crate::keychain::forget(credential_id) {
        Ok(()) => Response::Forgotten,
        Err(failure) => keychain_error(failure, "credential_forget_failed"),
      },
      Request::Clear {} => match crate::keychain::clear() {
        Ok(counts) => Response::Cleared {
          credential_count: counts.credential_count,
          identity_count: counts.identity_count,
        },
        Err(failure) => keychain_error(failure, "credential_clear_failed"),
      },
    }
  }
  #[cfg(not(target_os = "macos"))]
  {
    error(
      "credential_store_unsupported",
      "Saved SSH credentials are supported on macOS only.",
    )
  }
}

#[cfg(target_os = "macos")]
fn keychain_error(failure: crate::keychain::Error, fallback: &str) -> Response {
  if failure.is_busy() {
    error(
      "credential_store_busy",
      "Another Keychain request is still active. Complete or cancel it, then try again.",
    )
  } else if failure.is_missing_entitlement() {
    error(
      "credential_store_missing_entitlement",
      "This ctld process is not authorized for Keychain access. Use the signed ctld app with its matching provisioning profile.",
    )
  } else if failure.is_unavailable() {
    error(
      "credential_store_unavailable",
      "Keychain access is unavailable. Check your macOS login session and try again.",
    )
  } else if failure.is_locked() {
    error(
      "credential_store_locked",
      "Keychain access is locked or was not allowed.",
    )
  } else if failure.is_scan_limit() {
    error(
      "credential_discovery_limit",
      "There are too many owned Keychain entries to discover within the inventory limit.",
    )
  } else if failure.is_scan_conflict() {
    error(
      "credential_discovery_conflict",
      "Saved Keychain entries have duplicate identifiers and could not be listed safely.",
    )
  } else {
    error(
      fallback,
      if fallback == "credential_clear_failed" {
        "Could not clear every saved SSH credential. Some entries may already have been removed. Refresh and try again."
      } else {
        "Keychain could not complete the credential operation."
      },
    )
  }
}

#[cfg(test)]
mod tests;
