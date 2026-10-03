//! Conservative classification of OpenSSH control-master startup diagnostics.

use super::RequestError;

pub(super) fn failure(message: String) -> RequestError {
  if is_transport_failure(&message) {
    RequestError::MasterConnectionFailed(message)
  } else {
    RequestError::MasterFailed(message)
  }
}

fn is_transport_failure(message: &str) -> bool {
  let mut transport_failure = false;
  for line in message
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
  {
    if transport_diagnostic(line) || vpn_transport_diagnostic(line) {
      transport_failure = true;
    } else if !closure_diagnostic(line) && !debug_diagnostic(line) {
      // ProxyCommand diagnostics share stderr with OpenSSH. An authentication,
      // identity, host-key, or unknown failure must not become retryable merely
      // because OpenSSH subsequently reports that the proxy closed its pipe.
      return false;
    }
  }
  transport_failure
}

fn transport_diagnostic(line: &str) -> bool {
  // OpenSSH sshconnect.c emits this complete form after connect(2) fails.
  if let Some(detail) = line.strip_prefix("ssh: connect to host ")
    && let Some((endpoint, reason)) = detail.rsplit_once(": ")
  {
    return endpoint_has_port(endpoint) && transient_reason(reason);
  }
  // kex.c emits read errors while receiving the SSH identification string.
  if let Some(reason) = line.strip_prefix("kex_exchange_identification: read: ") {
    return transient_reason(reason);
  }
  if line == "Connection timed out during banner exchange" {
    return true;
  }
  // packet.c emits these forms for reset and timeout, independently of auth.
  if let Some(endpoint) = line.strip_prefix("Connection reset by ") {
    return endpoint_has_port(endpoint);
  }
  line
    .strip_prefix("Connection to ")
    .and_then(|detail| detail.strip_suffix(" timed out"))
    .is_some_and(endpoint_has_port)
}

fn transient_reason(reason: &str) -> bool {
  matches!(
    reason,
    "Connection refused"
      | "Connection reset by peer"
      | "Connection aborted"
      | "Software caused connection abort"
      | "No route to host"
      | "Network is unreachable"
      | "Network is down"
      | "Connection timed out"
      | "Operation timed out"
      | "Broken pipe"
  )
}

fn endpoint_has_port(endpoint: &str) -> bool {
  endpoint.rsplit_once(" port ").is_some_and(|(host, port)| {
    !host.is_empty()
      && !host.chars().any(char::is_whitespace)
      && port.parse::<u16>().is_ok_and(|port| port != 0)
  })
}

fn vpn_transport_diagnostic(line: &str) -> bool {
  matches!(
    line,
    "ctl-agent: remote operation failed: Remote VPN negotiation timed out"
      | "ctl-agent: remote operation failed: Remote VPN identity timed out"
      | "ctld: Remote VPN operation timed out"
      | "ctld: remote VPN I/O failed: unexpected end of file"
      | "ctld: remote VPN I/O failed: early eof"
      | "ctld: ctld I/O error: unexpected end of file"
      | "ctld: ctld I/O error: early eof"
      | "ctld: remote VPN SSH channel closed during protocol offer"
      | "ctld: remote VPN SSH channel closed during protocol selection"
      | "ctld: remote VPN SSH channel closed during request response"
      | "ctld: remote VPN SSH master disappeared; reconnect its owner"
      | "ctld: remote VPN operation failed (request_timeout): Remote VPN request timed out"
      | "ctld: remote VPN operation failed (vpn_connection_timeout): Remote VPN connection timed out"
  )
}

fn closure_diagnostic(line: &str) -> bool {
  // Closure alone cannot distinguish a network interruption from a broken
  // proxy configuration. Accept it only alongside recognized transport errors.
  line == "kex_exchange_identification: Connection closed by remote host"
    || line
      .strip_prefix("Connection closed by ")
      .is_some_and(endpoint_has_port)
}

fn debug_diagnostic(line: &str) -> bool {
  ["debug1: ", "debug2: ", "debug3: "]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

#[cfg(test)]
mod tests;
