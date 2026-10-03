use std::io::{self, IsTerminal as _};

use ctl_client::hosts::HostCatalogDocument;

use super::create::{Arguments, Request};
use super::{Error, edit};

pub(super) fn available() -> bool {
  io::stdin().is_terminal() && io::stderr().is_terminal()
}

pub(super) fn create(
  arguments: Arguments,
  document: &HostCatalogDocument,
) -> Result<Option<Request>, Error> {
  if !available() {
    return Err(Error::TerminalRequired);
  }
  let missing_name = arguments.name.is_none();
  let missing_destination = arguments.destination.is_none();
  let missing_method_name = arguments.method_name.is_none();
  let mut request = Request {
    name: arguments
      .name
      .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
    destination: arguments
      .destination
      .unwrap_or_else(|| "example.invalid".into()),
    method_name: arguments.method_name.unwrap_or_else(|| "SSH".into()),
    options: arguments.options,
  };
  // Reject invalid explicit settings before opening prompts. The placeholders
  // only validate missing fields; the completed request is validated again.
  validate(document, request.clone()).map_err(Error::Usage)?;
  cliclack::intro("Create a host")?;
  let result = (|| -> io::Result<Request> {
    if missing_name {
      let document = document.clone();
      let current = request.clone();
      let name: String = cliclack::input("Host name")
        .validate(move |value: &String| {
          let mut candidate = current.clone();
          candidate.name = value.trim().into();
          validate(&document, candidate)
        })
        .interact()?;
      request.name = name.trim().into();
    }
    if !request.options.ssh_config {
      let mut alias = request.clone();
      alias.destination = "example.invalid".into();
      alias.options.ssh_config = true;
      let mut selection =
        cliclack::select("Connection type").item(false, "Direct SSH", "Hostname or IP address");
      if validate(document, alias).is_ok() {
        selection = selection.item(true, "SSH config alias", "Use an entry from ~/.ssh/config");
      }
      request.options.ssh_config = selection.interact()?;
    }
    if missing_destination {
      let document = document.clone();
      let current = request.clone();
      let prompt = if request.options.ssh_config {
        "SSH config alias"
      } else {
        "SSH destination"
      };
      let destination: String = cliclack::input(prompt)
        .placeholder(if request.options.ssh_config {
          "work"
        } else {
          "alice@host.example.com"
        })
        .validate(move |value: &String| {
          let mut candidate = current.clone();
          candidate.destination = value.trim().into();
          validate(&document, candidate)
        })
        .interact()?;
      request.destination = destination.trim().into();
    } else {
      // The selected connection type may impose additional alias constraints
      // on a supplied destination or conflict with other explicit settings.
      validate(document, request.clone()).map_err(io::Error::other)?;
    }
    if !request.options.ssh_config {
      direct_options(&mut request, document)?;
    }
    if missing_method_name {
      let document = document.clone();
      let current = request.clone();
      let method_name: String = cliclack::input("Connection method name")
        .default_input("SSH")
        .validate(move |value: &String| {
          let mut candidate = current.clone();
          candidate.method_name = value.trim().into();
          validate(&document, candidate)
        })
        .interact()?;
      request.method_name = method_name.trim().into();
    }
    Ok(request)
  })();
  match result {
    Ok(request) => Ok(Some(request)),
    Err(error) if error.kind() == io::ErrorKind::Interrupted => {
      cliclack::outro_cancel("Cancelled. No changes made.")?;
      Ok(None)
    }
    Err(error) => Err(error.into()),
  }
}

fn direct_options(request: &mut Request, document: &HostCatalogDocument) -> io::Result<()> {
  if request.options.user.is_none() && !request.options.clear.contains(&edit::Clear::User) {
    let document = document.clone();
    let current = request.clone();
    let user: String = cliclack::input("Username (optional)")
      .required(false)
      .validate(move |value: &String| {
        let mut candidate = current.clone();
        candidate.options.user = optional(value);
        validate(&document, candidate)
      })
      .interact()?;
    request.options.user = optional(&user);
  }
  if request.options.port.is_none() && !request.options.clear.contains(&edit::Clear::Port) {
    let document = document.clone();
    let current = request.clone();
    let answer: String = cliclack::input("Port (optional)")
      .placeholder("22")
      .required(false)
      .validate(move |value: &String| {
        let mut candidate = current.clone();
        candidate.options.port = port(value)?;
        validate(&document, candidate)
      })
      .interact()?;
    request.options.port = port(&answer).map_err(io::Error::other)?;
  }
  if request.options.identity_file.is_none()
    && !request.options.clear.contains(&edit::Clear::IdentityFile)
  {
    let document = document.clone();
    let current = request.clone();
    let identity_file: String = cliclack::input("Identity file (optional)")
      .required(false)
      .validate(move |value: &String| {
        let mut candidate = current.clone();
        candidate.options.identity_file = optional(value);
        validate(&document, candidate)
      })
      .interact()?;
    request.options.identity_file = optional(&identity_file);
  }
  Ok(())
}

fn validate(document: &HostCatalogDocument, request: Request) -> Result<(), String> {
  edit::create(&mut document.clone(), request)
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn optional(value: &str) -> Option<String> {
  let value = value.trim();
  (!value.is_empty()).then(|| value.into())
}

fn port(value: &str) -> Result<Option<u16>, String> {
  let value = value.trim();
  if value.is_empty() {
    return Ok(None);
  }
  value
    .parse::<u16>()
    .ok()
    .filter(|value| *value != 0)
    .map(Some)
    .ok_or_else(|| "Enter a port from 1 to 65535, or leave this blank.".into())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn optional_port_retains_default_and_accepts_only_nonzero_u16() {
    assert_eq!(port("").unwrap(), None);
    assert_eq!(port("  ").unwrap(), None);
    assert_eq!(port(" 22 ").unwrap(), Some(22));
    assert_eq!(port("65535").unwrap(), Some(65535));
    for answer in ["0", "65536", "-1", "ssh", "22 22"] {
      assert!(port(answer).is_err());
    }
  }
}
