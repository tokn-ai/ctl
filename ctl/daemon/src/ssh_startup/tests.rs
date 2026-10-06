use super::*;

#[test]
fn transport_startup_failures_have_their_own_broker_error_code() {
  for message in [
    // Confirmed with a real local OpenSSH loopback connection attempt.
    "ssh: connect to host 127.0.0.1 port 1: Connection refused\r\n",
    "ssh: connect to host work port 22: No route to host",
    "ssh: connect to host 2001:db8::1 port 22: Network is unreachable",
    "ssh: connect to host work port 22: Network is down",
    "ssh: connect to host work port 22: Operation timed out",
    "kex_exchange_identification: read: Connection reset by peer",
    "Connection timed out during banner exchange",
    "Connection reset by 192.0.2.1 port 22",
    "Connection to 192.0.2.1 port 22 timed out",
    "debug1: Connecting to work [192.0.2.1] port 22.\nssh: connect to host work port 22: Connection refused",
  ] {
    let error = failure(message.into());
    assert_eq!(error.code(), "ssh_connection_failed", "{message}");
    assert!(matches!(error, RequestError::MasterConnectionFailed(detail) if detail == message));
  }
}

#[test]
fn a_failed_remote_vpn_proxy_does_not_hide_its_transport_failure() {
  for message in [
    "ctl-agent: remote operation failed: Remote VPN negotiation timed out\nctld: remote VPN I/O failed: unexpected end of file\nkex_exchange_identification: Connection closed by remote host\nConnection closed by UNKNOWN port 65535",
    "ctld: Remote VPN operation timed out\nConnection closed by UNKNOWN port 65535",
    "ctld: remote VPN SSH master disappeared; reconnect its owner\nConnection closed by UNKNOWN port 65535",
    "ctld: remote VPN SSH channel closed during request response\nConnection closed by UNKNOWN port 65535",
    "ctld: ctld I/O error: unexpected end of file\nConnection closed by UNKNOWN port 65535",
    "ctld: ctld I/O error: early eof\nConnection closed by UNKNOWN port 65535",
    "ctld: remote VPN operation failed (vpn_connection_timeout): Remote VPN connection timed out\nConnection closed by UNKNOWN port 65535",
  ] {
    assert_eq!(
      failure(message.into()).code(),
      "ssh_connection_failed",
      "{message}"
    );
  }
}

#[test]
fn authentication_security_configuration_and_unknown_errors_remain_fatal() {
  for message in [
    "",
    "exit status: 255",
    "alice@work: Permission denied (publickey,password).",
    "Host key verification failed.",
    "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!",
    "ssh: Could not resolve hostname work: Name or service not known",
    "ssh: connect to host work port 22: Permission denied",
    "ssh: connect to host work port 22: Operation not permitted",
    "ssh: connect to host work port 22: connection refused",
    "ssh: connect to host work port 22: Connection refused with extra text",
    "ssh: connect to host work port invalid: Connection refused",
    "ssh: connect to host work port 0: Connection refused",
    "kex_exchange_identification: banner line contains invalid characters",
    "Connection closed by UNKNOWN port 65535",
    "kex_exchange_identification: Connection closed by remote host",
    "ctld: Remote identity changed; no VPN credentials or requests were sent\nConnection closed by UNKNOWN port 65535",
    "ctld: No shared published remote VPN contract; update the remote components\nConnection closed by UNKNOWN port 65535",
    "ctld: The remote agent does not support VPN control; update its components\nConnection closed by UNKNOWN port 65535",
    "ctl-agent: remote operation failed: Remote VPN negotiation timed out\nHost key verification failed.",
    "alice@work: Permission denied (publickey,password).\nConnection reset by 192.0.2.1 port 22",
    "some arbitrary message about Connection refused",
  ] {
    let error = failure(message.into());
    assert_eq!(error.code(), "ssh_authentication_failed", "{message}");
    assert!(matches!(error, RequestError::MasterFailed(detail) if detail == message));
  }
}

#[test]
fn quiet_authentication_failures_request_interactive_authorization() {
  for message in [
    "alice@work: Permission denied (publickey,password).",
    "alice@2001:db8::1: Permission denied (publickey,keyboard-interactive).\r\n",
    "Permission denied (publickey).",
    "sign_and_send_pubkey: signing failed for ED25519 \"/fixture/id_ed25519\" from agent: agent refused operation\nalice@work: Permission denied (publickey).",
    "debug1: Offering public key: /fixture/id_ed25519\nsign_and_send_pubkey: signing failed for RSA \"fixture key\" from agent: agent refused operation\ndebug2: we did not send a packet, disable method\nalice@work: Permission denied (publickey,gssapi-with-mic).\ndebug1: No more authentication methods to try.",
  ] {
    assert!(
      matches!(
        failure_with_interaction(message.into(), false),
        RequestError::AuthenticationRequired
      ),
      "{message}"
    );
    assert!(
      matches!(
        failure_with_interaction(message.into(), true),
        RequestError::MasterFailed(detail) if detail == message
      ),
      "{message}"
    );
  }
}

#[test]
fn quiet_connections_preserve_transport_security_and_unknown_failures() {
  for message in [
    "ssh: connect to host work port 22: Connection refused",
    "ctld: Remote VPN operation timed out\nConnection closed by UNKNOWN port 65535",
  ] {
    assert!(
      matches!(
        failure_with_interaction(message.into(), false),
        RequestError::MasterConnectionFailed(detail) if detail == message
      ),
      "{message}"
    );
  }
  for message in [
    "",
    "exit status: 255",
    "sign_and_send_pubkey: signing failed for ED25519 \"fixture\" from agent: agent refused operation",
    "alice@work: Permission denied ().",
    "alice@work: Permission denied (publickey,).",
    "alice@work: Permission denied (publickey password).",
    "alice@work: Permission denied (publickey). extra text",
    "arbitrary prefix: Permission denied (publickey).",
    "Permission denied (publickey).\nunknown proxy failure",
    "unknown proxy failure\nalice@work: Permission denied (publickey).",
    "Host key verification failed.\nalice@work: Permission denied (publickey).",
    "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\nalice@work: Permission denied (publickey).",
    "ssh: Could not resolve hostname work: Name or service not known\nalice@work: Permission denied (publickey).",
    "alice@work: Permission denied (publickey).\nConnection reset by 192.0.2.1 port 22",
    "sign_and_send_pubkey: signing failed for ED25519 \"fixture\" from agent: unknown failure\nalice@work: Permission denied (publickey).",
    "alice@work: Permission denied (publickey).\nsign_and_send_pubkey: signing failed for ED25519 \"fixture\" from agent: agent refused operation",
  ] {
    assert!(
      matches!(
        failure_with_interaction(message.into(), false),
        RequestError::MasterFailed(detail) if detail == message
      ),
      "{message}"
    );
  }
}
