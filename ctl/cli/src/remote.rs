//! Update guidance for remote runtime failures.

use ctl_client::{CoreError, RemotePlatform};

#[derive(Debug, thiserror::Error)]
#[error("{reason} {guidance}")]
pub(super) struct UpdateRequired {
  reason: &'static str,
  guidance: String,
}

pub(super) fn update_required(
  error: &CoreError,
  platform: RemotePlatform,
  command: Option<&str>,
) -> Option<UpdateRequired> {
  if platform != RemotePlatform::Unix {
    return None;
  }
  let reason = match error {
    CoreError::AgentNotFound => "The remote ctl-agent is missing.",
    CoreError::IdentityUnsupported => "The remote ctl-agent predates host identity support.",
    CoreError::BrokerService(ctl_client::connection::Error::Daemon { code, .. })
      if code == "ctl_agent_not_found" =>
    {
      "The remote ctl-agent is missing."
    }
    CoreError::BrokerService(ctl_client::connection::Error::Daemon { code, .. })
      if code == "ctl_agent_update_required" =>
    {
      "The remote ctl-agent needs an update for shared SSH services."
    }
    CoreError::UnsupportedSshProtocol { marker } if marker == "ctl-ssh-v2" => {
      "The remote ctl-agent uses the older rmux protocol."
    }
    CoreError::UnsupportedSshProtocol { marker } if marker == "ctl-ssh-v3" => {
      "The remote ctl-agent predates published protocol contract negotiation."
    }
    _ => return None,
  };
  let guidance = command.map_or_else(
    || {
      "Use ctlup update from macOS or Linux with the same SSH account and route, then reconnect."
        .into()
    },
    |command| format!("Update this host from macOS or Linux, then reconnect.\n{command}"),
  );
  Some(UpdateRequired { reason, guidance })
}

pub(super) fn update_command(
  host: Option<&str>,
  destination: Option<&str>,
  method: Option<&str>,
) -> Option<String> {
  match (host, destination, method) {
    (Some(host), None, method) => {
      let host = quoted_selector(host)?;
      let mut command = format!("ctlup update --host={host}");
      if let Some(method) = method {
        command.push_str(" --method=");
        command.push_str(&quoted_selector(method)?);
      }
      Some(command)
    }
    (None, Some(destination), None) => {
      let address = destination.strip_prefix("ssh://").unwrap_or(destination);
      if address
        .rsplit_once('@')
        .is_some_and(|(userinfo, _)| userinfo.contains(':'))
      {
        return None;
      }
      Some(format!(
        "ctlup update --destination={}",
        quoted_selector(destination)?
      ))
    }
    _ => None,
  }
}

fn quoted_selector(value: &str) -> Option<String> {
  // Escaping a terminal control for display would change the selector when
  // copied into a shell. Omit the whole command when it cannot be shown exactly.
  (crate::table::text(value) == value).then(|| crate::connection::shell_quote(value))
}

#[cfg(test)]
mod tests;
