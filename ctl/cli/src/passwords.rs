//! Local SSH credential metadata and explicit removal through the signed helper.

mod inventory;

#[cfg(test)]
mod tests;

use std::io::{self, IsTerminal as _};

use ctl_client::local_credentials::{self as helper, Error as HelperError};
use ctl_ipc::{credentials, identities};
use inventory::{Choice, Entry, Removal, Snapshot};

#[derive(Debug, clap::Subcommand)]
pub enum Command {
  /// Discover saved SSH passwords and identity passphrases without reading secrets.
  List,
  /// Show a saved entry's metadata, without revealing its secret.
  Show {
    /// Short ID from the table, full ID, unique full-ID prefix, or exact unique name.
    selector: String,
  },
  /// Remove one saved secret after confirmation; private key files are retained.
  Remove {
    /// Omit to choose a saved entry interactively.
    selector: Option<String>,
  },
  /// Clear all saved SSH passwords and key passphrases after confirmation.
  Clear,
}

pub async fn run(command: Option<Command>, json: bool) -> Result<(), Error> {
  if !cfg!(target_os = "macos") {
    return Err(Error::Unsupported);
  }
  let result = tokio::select! {
    result = run_inner(command.unwrap_or(Command::List), json) => result,
    signal = tokio::signal::ctrl_c() => match signal {
      Ok(()) => Err(Error::Cancelled),
      Err(error) => Err(Error::Io(error)),
    },
  };
  // main exits the process after this returns. Let cancelled helper exchanges
  // finish killing and reaping their children before that can stop the runtime.
  helper::drain_exchanges().await?;
  result
}

async fn run_inner(command: Command, json: bool) -> Result<(), Error> {
  if matches!(command, Command::Remove { .. } | Command::Clear) {
    require_terminal()?;
  }
  let snapshot = read_inventory().await?;
  match command {
    Command::List => {
      if json {
        print_json(&snapshot)?;
      } else {
        println!("{}", snapshot.render_list());
        print_warnings(&snapshot);
      }
    }
    Command::Show { selector } => {
      let entry = snapshot.select(&selector).map_err(Error::Usage)?;
      if json {
        print_json(entry)?;
      } else {
        println!("{}", snapshot.render_show(entry));
        print_warnings(&snapshot);
      }
    }
    Command::Remove { selector } => {
      print_warnings(&snapshot);
      let entry = if let Some(selector) = selector {
        snapshot.select(&selector).map_err(Error::Usage)?.clone()
      } else {
        let Some(selector) = pick(snapshot.choices()).await? else {
          return cancelled(json);
        };
        snapshot.select(&selector).map_err(Error::Usage)?.clone()
      };
      eprintln!("{}", snapshot.render_show(&entry));
      if !confirm(format!(
        "Remove {} ({})?",
        crate::table::text(&entry.name),
        snapshot.short_id(&entry),
      ))
      .await?
      {
        return cancelled(json);
      }
      // A password can be replaced while the confirmation is open. Refuse to
      // remove a newer item on the basis of an older metadata preview.
      let current = read_inventory().await?;
      check_unchanged(&entry, &current)?;
      remove(&entry).await?;
      if json {
        print_json(&serde_json::json!({ "removed_id": entry.id }))?;
      } else {
        println!("Removed {}.", crate::table::text(&entry.name));
      }
    }
    Command::Clear => {
      eprintln!("{}", snapshot.render_list());
      print_warnings(&snapshot);
      if !confirm("Clear all saved SSH passwords and key passphrases?".into()).await? {
        return cancelled(json);
      }
      match helper::request_credentials(credentials::Request::Clear {}).await? {
        credentials::Response::Cleared {
          credential_count,
          identity_count,
        } => {
          if json {
            print_json(&serde_json::json!({
              "credential_count": credential_count,
              "identity_count": identity_count,
            }))?;
          } else {
            println!(
              "Cleared {credential_count} SSH credentials and {identity_count} key passphrases."
            );
          }
        }
        _ => return Err(Error::UnexpectedResponse),
      }
    }
  }
  Ok(())
}

async fn read_inventory() -> Result<Snapshot, Error> {
  match helper::request_credentials(credentials::Request::Discover {}).await? {
    credentials::Response::Discovered { inventory } => Ok(inventory::build(inventory)),
    _ => Err(Error::UnexpectedResponse),
  }
}

async fn remove(entry: &Entry) -> Result<(), Error> {
  match entry.removal() {
    Removal::Credential(credential_id) => {
      match helper::request_credentials(credentials::Request::Forget {
        credential_id: credential_id.into(),
      })
      .await?
      {
        credentials::Response::Forgotten => Ok(()),
        _ => Err(Error::UnexpectedResponse),
      }
    }
    Removal::Identity(identity_id) => {
      match helper::request_identity(identities::Request::Forget {
        identity_id: identity_id.into(),
      })
      .await?
      {
        identities::Response::Forgotten => Ok(()),
        _ => Err(Error::UnexpectedResponse),
      }
    }
  }
}

fn check_unchanged(entry: &Entry, current: &Snapshot) -> Result<(), Error> {
  match current.select(&entry.id) {
    Ok(latest) if latest == entry => Ok(()),
    _ => Err(Error::Usage(
      "The selected saved entry changed or could not be checked. List it again before removing it."
        .into(),
    )),
  }
}

fn require_terminal() -> Result<(), Error> {
  if io::stdin().is_terminal() && io::stderr().is_terminal() {
    Ok(())
  } else {
    Err(Error::TerminalRequired)
  }
}

async fn confirm(question: String) -> Result<bool, Error> {
  tokio::task::spawn_blocking(move || {
    let result = cliclack::confirm(question).initial_value(false).interact();
    match result {
      Ok(confirmed) => Ok(confirmed),
      Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(false),
      Err(error) => Err(Error::Io(error)),
    }
  })
  .await
  .map_err(|_| Error::WorkerStopped)?
}

async fn pick(entries: Vec<Choice>) -> Result<Option<String>, Error> {
  if entries.is_empty() {
    return Err(Error::Usage(
      "No listed saved entries are available to remove.".into(),
    ));
  }
  tokio::task::spawn_blocking(move || {
    let mut picker = cliclack::select("Remove a saved password or key passphrase");
    for entry in entries {
      picker = picker.item(
        entry.id,
        crate::table::text(&entry.name),
        crate::table::text(&entry.hint),
      );
    }
    match picker.interact() {
      Ok(selector) => Ok(Some(selector)),
      Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(None),
      Err(error) => Err(Error::Io(error)),
    }
  })
  .await
  .map_err(|_| Error::WorkerStopped)?
}

fn print_warnings(snapshot: &Snapshot) {
  for warning in &snapshot.warnings {
    eprintln!("Warning: {}", crate::table::text(warning));
  }
}

fn print_json(value: &impl serde::Serialize) -> Result<(), Error> {
  println!("{}", serde_json::to_string_pretty(value)?);
  Ok(())
}

fn cancelled(json: bool) -> Result<(), Error> {
  if json {
    print_json(&serde_json::json!({ "cancelled": true }))?;
  } else {
    eprintln!("Cancelled. No saved passwords removed.");
  }
  Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("Saved SSH passwords and key passphrases require macOS Keychain.")]
  Unsupported,
  #[error("Password removal requires an interactive terminal for confirmation.")]
  TerminalRequired,
  #[error("Password operation cancelled.")]
  Cancelled,
  #[error("The password confirmation worker stopped.")]
  WorkerStopped,
  #[error("The credential helper returned an unexpected response. Update ctld and try again.")]
  UnexpectedResponse,
  #[error("{0}")]
  Usage(String),
  #[error(transparent)]
  Helper(#[from] HelperError),
  #[error("Could not complete the password terminal operation.")]
  Io(#[source] io::Error),
  #[error("Could not encode password metadata.")]
  Json(#[from] serde_json::Error),
}
