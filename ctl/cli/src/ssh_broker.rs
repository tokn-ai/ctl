use std::io::{self, Write as _};
use std::path::PathBuf;

use ctl_ipc::{ClientMessage, PromptKind, ServerMessage, SshTarget};
use zeroize::Zeroizing;

pub async fn ensure_master(target: SshTarget) -> Result<PathBuf, Error> {
  let mut stream = ctl_ipc::connect_or_start_daemon().await?;
  handshake(&mut stream).await?;
  ctl_ipc::write_frame(&mut stream, &ClientMessage::EnsureMaster { target }).await?;
  loop {
    match ctl_ipc::read_frame::<_, ServerMessage>(&mut stream).await? {
      Some(ServerMessage::Prompt {
        prompt_id,
        kind,
        message,
        warning,
      }) => {
        let response = tokio::task::spawn_blocking(move || {
          if let Some(warning) = warning {
            eprintln!("Warning: {warning}");
          }
          prompt(kind, &message)
        })
        .await
        .map_err(|_| Error::PromptWorkerStopped)??;
        ctl_ipc::write_frame(
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

pub async fn request(message: ClientMessage) -> Result<ServerMessage, Error> {
  let mut stream = ctl_ipc::connect_or_start_daemon().await?;
  handshake(&mut stream).await?;
  ctl_ipc::write_frame(&mut stream, &message).await?;
  match ctl_ipc::read_frame::<_, ServerMessage>(&mut stream).await? {
    Some(ServerMessage::Error { code, message }) => Err(Error::Daemon { code, message }),
    Some(message) => Ok(message),
    None => Err(Error::ConnectionClosed),
  }
}

/// Exchange a passive observation without starting ctld or authenticating SSH.
pub async fn request_existing(message: ClientMessage) -> Result<Option<ServerMessage>, Error> {
  let exchange = async {
    let mut stream = match ctl_ipc::connect_existing().await {
      Ok(stream) => stream,
      Err(ctl_ipc::ConnectError::Connect(error))
        if matches!(
          error.kind(),
          io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        ) =>
      {
        return Ok(None);
      }
      Err(error) => return Err(error.into()),
    };
    handshake(&mut stream).await?;
    ctl_ipc::write_frame(&mut stream, &message).await?;
    let response = ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
      .await?
      .ok_or(Error::ConnectionClosed)?;
    Ok(Some(response))
  };
  tokio::time::timeout(std::time::Duration::from_secs(15), exchange)
    .await
    .map_err(|_| Error::StatusTimeout)?
}

async fn handshake(stream: &mut ctl_ipc::Stream) -> Result<(), Error> {
  ctl_ipc::write_frame(
    stream,
    &ClientMessage::Handshake {
      protocol_version: ctl_ipc::PROTOCOL_VERSION,
    },
  )
  .await?;
  match ctl_ipc::read_frame::<_, ServerMessage>(stream).await? {
    Some(ServerMessage::HandshakeAccepted { protocol_version })
      if protocol_version == ctl_ipc::PROTOCOL_VERSION =>
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
  use std::io::BufRead as _;
  io::stderr().flush().map_err(Error::Prompt)?;
  // stdin may be an scp protocol stream or the input to ctl exec. Prompts must
  // never consume those bytes or wait forever for binary input to end.
  let terminal = std::fs::File::open("/dev/tty").map_err(Error::Prompt)?;
  let mut response = Zeroizing::new(String::new());
  io::BufReader::new(terminal)
    .read_line(&mut response)
    .map_err(Error::Prompt)?;
  while response.ends_with(['\n', '\r']) {
    response.pop();
  }
  Ok(std::mem::take(&mut *response))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("ctld status query timed out")]
  StatusTimeout,
  #[error(transparent)]
  Connect(#[from] ctl_ipc::ConnectError),
  #[error(transparent)]
  Codec(#[from] ctl_ipc::CodecError),
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
