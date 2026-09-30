use super::{IdentityError, files, inspect_path, inventory};
#[cfg(unix)]
use super::{LocalAgent, save_verified};
use ctld_ipc::identities::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Request, Response};
use std::io::{self, Read, Write};
use zeroize::Zeroizing;

/// Handle one bounded request without connecting to or starting the daemon.
///
/// # Errors
/// Returns only input/output failures, without including request contents.
pub fn run(reader: impl Read, mut writer: impl Write) -> io::Result<()> {
  let mut bytes = Zeroizing::new(Vec::new());
  reader
    .take((MAX_REQUEST_BYTES + 1) as u64)
    .read_to_end(&mut bytes)?;
  let request = if bytes.len() <= MAX_REQUEST_BYTES {
    serde_json::from_slice::<Request>(&bytes).ok()
  } else {
    None
  };
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;
  let response = match request {
    Some(request) => runtime
      .block_on(handle(request))
      .unwrap_or_else(error_response),
    None => error_response(IdentityError::InvalidRequest),
  };
  let mut response = response;
  let encoded = loop {
    let encoded = serde_json::to_vec(&response).map_err(io::Error::other)?;
    if encoded.len() < MAX_RESPONSE_BYTES {
      break encoded;
    }
    if let Response::Inventory { inventory } = &mut response {
      inventory
        .identity_files
        .truncate(inventory.identity_files.len() / 2);
      inventory.complete = false;
      inventory.warning = Some("The identity inventory exceeded its size limit.".into());
    } else {
      return Err(io::Error::other(
        "identity response exceeded its size limit",
      ));
    }
  };
  writer.write_all(&encoded)?;
  writer.write_all(b"\n")?;
  writer.flush()
}

fn error_response(error: IdentityError) -> Response {
  Response::Error {
    code: error.code().into(),
    message: error.to_string(),
  }
}

async fn handle(request: Request) -> Result<Response, IdentityError> {
  match request {
    Request::List { paths } => Ok(Response::Inventory {
      inventory: inventory::list(&paths)?,
    }),
    Request::Save {
      path,
      file_version,
      passphrase,
    } => {
      if !files::valid_id(&file_version) {
        return Err(IdentityError::InvalidRequest);
      }
      let snapshot = inspect_path(&path)?;
      if snapshot.file_version != file_version {
        return Err(IdentityError::FileChanged);
      }
      if !snapshot.encrypted {
        return Err(IdentityError::InvalidRequest);
      }
      #[cfg(unix)]
      {
        let mut agent = LocalAgent::start().await?;
        let verified = agent.add_identity(&snapshot, passphrase.clone()).await?;
        save_verified(&snapshot, &verified, &passphrase)?;
        Ok(Response::Saved)
      }
      #[cfg(not(unix))]
      {
        let _ = passphrase;
        Err(IdentityError::KeychainUnavailable)
      }
    }
    Request::Forget { identity_id } => {
      if !files::valid_id(&identity_id) {
        return Err(IdentityError::InvalidRequest);
      }
      #[cfg(target_os = "macos")]
      {
        crate::keychain::identity::forget(&identity_id)?;
        Ok(Response::Forgotten)
      }
      #[cfg(not(target_os = "macos"))]
      Err(IdentityError::KeychainUnavailable)
    }
  }
}
