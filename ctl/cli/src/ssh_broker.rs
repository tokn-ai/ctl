use std::io::{self, Write as _};
use std::path::PathBuf;

use ctld_ipc::{ClientMessage, PromptKind, ServerMessage, SshTarget};
use zeroize::Zeroizing;

pub async fn ensure_master(target: SshTarget) -> Result<PathBuf, Error> {
  let mut stream = ctld_ipc::connect_or_start_daemon().await?;
  handshake(&mut stream).await?;
  ctld_ipc::write_frame(&mut stream, &ClientMessage::EnsureMaster { target }).await?;
  loop {
    match ctld_ipc::read_frame::<_, ServerMessage>(&mut stream).await? {
      Some(ServerMessage::Prompt {
        prompt_id,
        kind,
        message,
      }) => {
        let response = tokio::task::spawn_blocking(move || prompt(kind, &message))
          .await
          .map_err(|_| Error::PromptWorkerStopped)??;
        ctld_ipc::write_frame(
          &mut stream,
          &ClientMessage::PromptResponse {
            prompt_id,
            response,
          },
        )
        .await?;
      }
      Some(ServerMessage::MasterReady { control_path }) => return Ok(control_path),
      Some(ServerMessage::AuthenticationRequired) => return Err(Error::AuthenticationRequired),
      Some(ServerMessage::Error { code, message }) => return Err(Error::Daemon { code, message }),
      Some(_) => return Err(Error::UnexpectedResponse),
      None => return Err(Error::ConnectionClosed),
    }
  }
}

async fn handshake(stream: &mut ctld_ipc::Stream) -> Result<(), Error> {
  ctld_ipc::write_frame(
    stream,
    &ClientMessage::Handshake {
      protocol_version: ctld_ipc::PROTOCOL_VERSION,
    },
  )
  .await?;
  match ctld_ipc::read_frame::<_, ServerMessage>(stream).await? {
    Some(ServerMessage::HandshakeAccepted { protocol_version })
      if protocol_version == ctld_ipc::PROTOCOL_VERSION =>
    {
      Ok(())
    }
    _ => Err(Error::UnexpectedResponse),
  }
}

fn prompt(kind: PromptKind, message: &str) -> Result<Option<Zeroizing<String>>, Error> {
  match kind {
    PromptKind::Secret => rpassword::prompt_password(format!("{message} "))
      .map(Zeroizing::new)
      .map(Some)
      .map_err(Error::Prompt),
    PromptKind::Confirm => {
      eprint!("{message} ");
      read_response().map(|response| Some(Zeroizing::new(response)))
    }
    PromptKind::CredentialSave => {
      eprintln!("{message}");
      eprint!("Save credential? [yes/no/never] ");
      read_response().map(|response| Some(Zeroizing::new(response.to_lowercase())))
    }
    PromptKind::CredentialSaveError => {
      eprintln!("{message}");
      eprint!("Press Enter to continue. ");
      read_response().map(|_| Some(Zeroizing::new("confirm".into())))
    }
  }
}

fn read_response() -> Result<String, Error> {
  io::stderr().flush().map_err(Error::Prompt)?;
  let mut response = Zeroizing::new(String::new());
  io::stdin()
    .read_line(&mut response)
    .map_err(Error::Prompt)?;
  while response.ends_with(['\n', '\r']) {
    response.pop();
  }
  Ok(std::mem::take(&mut *response))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error(transparent)]
  Connect(#[from] ctld_ipc::ConnectError),
  #[error(transparent)]
  Codec(#[from] ctld_ipc::CodecError),
  #[error("ctld closed the SSH authentication request")]
  ConnectionClosed,
  #[error("ctld returned an unexpected response")]
  UnexpectedResponse,
  #[error("ctld requires a new explicit SSH authentication attempt")]
  AuthenticationRequired,
  #[error("ctld error {code}: {message}")]
  Daemon { code: String, message: String },
  #[error("could not read an SSH response: {0}")]
  Prompt(#[source] io::Error),
  #[error("the SSH prompt reader stopped unexpectedly")]
  PromptWorkerStopped,
}
