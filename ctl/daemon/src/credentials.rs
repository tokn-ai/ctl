//! One-shot, metadata-only access through ctld's signed Keychain identity.

use ctld_ipc::credentials::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Request, Response};
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
      Request::Forget { credential_id } => match crate::keychain::forget(credential_id) {
        Ok(()) => Response::Forgotten,
        Err(failure) => keychain_error(failure, "credential_forget_failed"),
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
      "credential_store_unavailable",
      "The selected ctld needs its signed Keychain entitlement to manage saved credentials.",
    )
  } else if matches!(
    failure.0.code(),
    -25_308 | -25_315 | -25_293 | -128 | -25_291
  ) {
    error(
      "credential_store_locked",
      "Keychain access is locked, unavailable, or was not allowed.",
    )
  } else {
    error(
      fallback,
      "Keychain could not complete the credential operation.",
    )
  }
}

#[cfg(test)]
mod tests;
