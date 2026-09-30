mod edit;
mod status;

use ctl_core::hosts::{self, HostCatalogDocument, HostError, WorkspaceHost};
use edit::ConnectionOptions;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
  /// List saved hosts and passive status for their connection methods.
  List {
    #[arg(long)]
    json: bool,
  },
  /// Show a saved host's settings, identity, methods, and passive status.
  Show {
    #[arg(id = "saved_host", value_name = "HOST")]
    host: String,
    #[arg(long)]
    json: bool,
  },
  /// Show passive SSH connection status for one or all saved hosts.
  Status {
    #[arg(id = "saved_host", value_name = "HOST")]
    host: Option<String>,
    #[arg(long)]
    json: bool,
  },
  /// Save a host with its first SSH connection method, without connecting.
  Add {
    name: String,
    destination: String,
    #[arg(long, default_value = "SSH")]
    method_name: String,
    #[command(flatten)]
    options: ConnectionOptions,
    #[arg(long)]
    json: bool,
  },
  /// Rename a host or update its selected (by default preferred) method.
  Update {
    #[arg(id = "saved_host", value_name = "HOST")]
    host: String,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    destination: Option<String>,
    #[command(flatten)]
    options: ConnectionOptions,
    #[arg(long)]
    json: bool,
  },
  /// Remove a saved definition; existing connections and remote sessions remain.
  Remove {
    #[arg(id = "saved_host", value_name = "HOST")]
    host: String,
    #[arg(long)]
    json: bool,
  },
  /// Manage alternate routes and the preferred connection method.
  Method {
    #[command(subcommand)]
    command: MethodCommand,
  },
  /// Authenticate the preferred (or --method) SSH connection without a shell.
  #[cfg(unix)]
  Connect {
    #[arg(id = "saved_host", value_name = "HOST")]
    host: String,
  },
  /// Disconnect all saved methods, or only --method; active channels may close.
  #[cfg(unix)]
  Disconnect {
    #[arg(id = "saved_host", value_name = "HOST")]
    host: String,
  },
}

#[derive(Debug, clap::Subcommand)]
pub enum MethodCommand {
  /// Add an alternate connection method to a saved host.
  Add {
    #[arg(id = "saved_host", value_name = "HOST")]
    host: String,
    name: String,
    destination: String,
    #[arg(long)]
    prefer: bool,
    #[command(flatten)]
    options: ConnectionOptions,
    #[arg(long)]
    json: bool,
  },
  /// Update a connection method by name or ID.
  Update {
    #[arg(id = "saved_host", value_name = "HOST")]
    host: String,
    method_name: String,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    destination: Option<String>,
    #[command(flatten)]
    options: ConnectionOptions,
    #[arg(long)]
    json: bool,
  },
  /// Remove a non-preferred method; select another preferred method first.
  Remove {
    #[arg(id = "saved_host", value_name = "HOST")]
    host: String,
    method_name: String,
    #[arg(long)]
    json: bool,
  },
  /// Choose the method used by default for this host.
  Prefer {
    #[arg(id = "saved_host", value_name = "HOST")]
    host: String,
    method_name: String,
    #[arg(long)]
    json: bool,
  },
}

pub async fn run(command: Command, method: Option<&str>) -> Result<(), Error> {
  let path = crate::target::catalog_path()?;
  let snapshot = hosts::storage::load(&path)?;
  match command {
    Command::List { json } => {
      reject_method(method)?;
      status::display(&snapshot.document, None, None, json, false).await
    }
    Command::Show { host, json } => {
      status::display(&snapshot.document, Some(&host), method, json, true).await
    }
    Command::Status { host, json } => {
      if host.is_none() {
        reject_method(method)?;
      }
      status::display(&snapshot.document, host.as_deref(), method, json, false).await
    }
    #[cfg(unix)]
    Command::Connect { host } => status::connect(&snapshot.document, &host, method).await,
    #[cfg(unix)]
    Command::Disconnect { host } => status::disconnect(&snapshot.document, &host, method).await,
    command => {
      let mut document = snapshot.document;
      let (host, json) = edit::apply(&mut document, command, method)?;
      hosts::storage::update(&path, snapshot.revision.as_deref(), document)?;
      if json {
        println!("{}", serde_json::to_string_pretty(&host)?);
      } else {
        println!("{}\t{}", host.host_id, host.name);
      }
      Ok(())
    }
  }
}

fn reject_method(method: Option<&str>) -> Result<(), Error> {
  if method.is_some() {
    Err(Error::Usage("--method applies to host show/status/update/connect/disconnect; method management takes a positional method name or ID.".into()))
  } else {
    Ok(())
  }
}

fn host_index(catalog: &HostCatalogDocument, selector: &str) -> Result<usize, Error> {
  if let Some(index) = catalog
    .hosts
    .iter()
    .position(|host| host.host_id == selector)
  {
    return Ok(index);
  }
  let mut matches = catalog
    .hosts
    .iter()
    .enumerate()
    .filter(|(_, host)| host.name == selector);
  let Some((index, _)) = matches.next() else {
    return Err(Error::Usage(format!(
      "No saved host matches {selector:?}. Use ctl host add to save it."
    )));
  };
  if matches.next().is_some() {
    return Err(Error::Usage(format!(
      "More than one saved host matches {selector:?}; use its host ID."
    )));
  }
  Ok(index)
}

fn method_index(host: &WorkspaceHost, selector: Option<&str>) -> Result<usize, Error> {
  let selector = selector
    .or(host.preferred_method_id.as_deref())
    .unwrap_or_default();
  if let Some(index) = host
    .connection_methods
    .iter()
    .position(|method| method.method_id == selector)
  {
    return Ok(index);
  }
  let mut matches = host
    .connection_methods
    .iter()
    .enumerate()
    .filter(|(_, method)| method.name == selector);
  let Some((index, _)) = matches.next() else {
    return Err(Error::Usage(format!(
      "No connection method matches {selector:?}."
    )));
  };
  if matches.next().is_some() {
    return Err(Error::Usage(format!(
      "More than one connection method matches {selector:?}; use its method ID."
    )));
  }
  Ok(index)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error(transparent)]
  Host(#[from] HostError),
  #[error(transparent)]
  Json(#[from] serde_json::Error),
  #[cfg(unix)]
  #[error(transparent)]
  Broker(#[from] crate::ssh_broker::Error),
  #[cfg(unix)]
  #[error(transparent)]
  Vpn(#[from] crate::target::Error),
  #[error("{0}")]
  Usage(String),
}
