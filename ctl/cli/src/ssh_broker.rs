//! Terminal presentation adapter for the shared connection client.
use ctl_client::connection::{ConnectionClient, InteractionPolicy};
pub use ctl_client::connection::{Error, check_route_support, request};
use ctl_ipc::{PromptKind, SshTarget};
use std::io::{self, Write as _};
use std::path::PathBuf;
use zeroize::Zeroizing;

pub async fn ensure_master(target: SshTarget) -> Result<PathBuf, Error> {
  ensure_master_with_interaction(target, true).await
}

pub async fn ensure_master_with_interaction(
  target: SshTarget,
  interactive: bool,
) -> Result<PathBuf, Error> {
  MasterClient::default()
    .ensure_master(target, interactive)
    .await
}

#[derive(Default)]
pub struct MasterClient(ConnectionClient);

impl MasterClient {
  pub async fn ensure_master(
    &self,
    target: SshTarget,
    interactive: bool,
  ) -> Result<PathBuf, Error> {
    let interaction = if interactive {
      InteractionPolicy::Interactive
    } else {
      InteractionPolicy::Quiet
    };
    self
      .0
      .ensure(target, interaction, interactive_prompt)
      .await
      .map(|ready| ready.control_path)
  }
}

async fn interactive_prompt(
  kind: PromptKind,
  message: String,
  warning: Option<String>,
) -> Result<Option<Zeroizing<String>>, Error> {
  tokio::task::spawn_blocking(move || {
    if let Some(warning) = warning {
      eprintln!("Warning: {warning}");
    }
    prompt(kind, &message)
  })
  .await
  .map_err(|_| Error::PromptWorkerStopped)?
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
