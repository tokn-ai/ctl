use std::path::PathBuf;

use ctl_client::hosts;

use super::{Error, edit, questionnaire};

#[derive(Debug, clap::Args)]
pub struct Arguments {
  /// Saved host name; prompted when omitted.
  pub(super) name: Option<String>,
  /// SSH destination or config alias; prompted when omitted.
  pub(super) destination: Option<String>,
  /// Name for the first connection method (defaults to SSH).
  #[arg(long)]
  pub(super) method_name: Option<String>,
  #[command(flatten)]
  pub(super) options: edit::ConnectionOptions,
  /// Print the saved host as JSON, keeping interactive prompts on stderr.
  #[arg(long)]
  pub(super) json: bool,
}

#[derive(Clone)]
pub(super) struct Request {
  pub name: String,
  pub destination: String,
  pub method_name: String,
  pub options: edit::ConnectionOptions,
}

pub(super) async fn run(arguments: Arguments, path: PathBuf) -> Result<(), Error> {
  let interactive = arguments.name.is_none() || arguments.destination.is_none();
  if interactive && !questionnaire::available() {
    return Err(Error::TerminalRequired);
  }
  let json = arguments.json;
  let host = tokio::task::spawn_blocking(move || -> Result<_, Error> {
    let request = if interactive {
      let snapshot = hosts::storage::load(&path)?;
      let Some(request) = questionnaire::create(arguments, &snapshot.document)? else {
        return Ok(None);
      };
      request
    } else {
      Request {
        name: arguments.name.expect("explicit host name"),
        destination: arguments.destination.expect("explicit SSH destination"),
        method_name: arguments.method_name.unwrap_or_else(|| "SSH".into()),
        options: arguments.options,
      }
    };
    // Reload after prompting so edits made while the questionnaire was open
    // are preserved. The revision check also catches an edit during this save.
    let snapshot = hosts::storage::load(&path)?;
    let mut document = snapshot.document;
    let host = edit::create(&mut document, request)?;
    hosts::storage::update(&path, snapshot.revision.as_deref(), document)?;
    if interactive {
      cliclack::outro("Host saved.")?;
    }
    Ok(Some(host))
  })
  .await
  .map_err(|_| Error::QuestionnaireWorkerStopped)??;
  if let Some(host) = host {
    super::display_saved(host, json)?;
  }
  Ok(())
}
