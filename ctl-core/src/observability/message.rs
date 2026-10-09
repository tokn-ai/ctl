//! Human descriptions are derived from typed metadata, never raw requests/errors.
use super::{Event, Outcome, Record};
use std::fmt::Write as _;

impl Event {
  fn description(self) -> &'static str {
    match self {
      Self::DaemonLifecycle => "Daemon lifecycle",
      Self::HelperRequest => "Helper request",
      Self::ProxyConnection => "Proxy connection",
      Self::Connection => "Open SSH connection",
      Self::ConnectionRequest => "Ensure SSH connection",
      Self::ConnectionReuse => "Reuse existing SSH connection",
      Self::Disconnect => "Host disconnect",
      Self::CredentialRead => "Read saved credential",
      Self::CredentialSave => "Save credential",
      Self::CredentialRemove => "Remove saved credential",
      Self::CredentialClear => "Clear saved credentials",
      Self::CredentialInventory => "Inspect saved credentials",
      Self::IdentitySave => "Save SSH identity",
      Self::IdentityRemove => "Remove SSH identity",
      Self::IdentityInventory => "Inspect SSH identities",
      Self::SessionCreate => "Create session",
      Self::SessionTerminate => "Terminate session",
      Self::SessionMerge => "Merge sessions",
      Self::PaneSplit => "Split pane",
      Self::PanePromote => "Promote pane to session",
      Self::PaneKill => "Terminate pane process",
      Self::PaneExit => "Pane process exited",
      Self::PaneResize => "Resize pane",
      Self::ViewResize => "Resize terminal canvas",
      Self::DividerResize => "Move pane divider",
      Self::PaneZoom => "Change pane zoom",
      Self::ViewUpdate => "Update session layout",
      Self::AttachmentCreate => "Attach to session",
      Self::AttachmentResume => "Resume session attachment",
      Self::AttachmentSuspend => "Attachment disconnected; reconnect grace started",
      Self::AttachmentExpire => "Attachment reconnect grace expired",
      Self::AttachmentDetach => "Attachment detached",
      Self::LeaseAcquire => "Acquire attachment lease",
      Self::LeaseRelease => "Release attachment lease",
      Self::SessionTransport => "Session transport",
      Self::ControlTransport => "Daemon control transport",
      Self::LogConfiguration => "Invalid CTL_LOG_LEVEL; using info",
      Self::VpnMonitor => "VPN monitoring failed; cleaning up connection",
    }
  }
}

impl Record {
  /// A safe human description, also available for older records.
  #[must_use]
  pub fn message(&self) -> String {
    let description = self.event.description();
    let mut message = match self.outcome {
      Outcome::Started => format!("{description}: started"),
      Outcome::Succeeded
        if matches!(
          self.event,
          Event::PaneExit
            | Event::AttachmentSuspend
            | Event::AttachmentExpire
            | Event::AttachmentDetach
        ) =>
      {
        description.to_owned()
      }
      Outcome::Succeeded => format!("{description}: completed"),
      Outcome::Missing => format!("{description}: unavailable"),
      Outcome::Failed if matches!(self.event, Event::LogConfiguration | Event::VpnMonitor) => {
        description.to_owned()
      }
      Outcome::Failed => format!("{description}: failed"),
      Outcome::Interrupted => format!("{description}: interrupted"),
    };
    if let Some(code) = self.error_code.as_deref() {
      message.push_str("; ");
      message.push_str(match code {
        "daemon_runtime_directory_failed" => "could not prepare the private runtime directory",
        "daemon_bind_failed" => "could not bind the daemon endpoint",
        "daemon_accept_failed" => "could not accept a client connection",
        "daemon_shutdown_signal_failed" => "could not install shutdown handlers",
        "daemon_platform_unsupported" => "daemon is unavailable on this platform",
        "daemon_already_running" => "another daemon already owns this endpoint",
        "daemon_control_endpoint_failed" => "could not derive the control endpoint",
        "daemon_io_failed" => "daemon I/O failed",
        "attachment_timeout_invalid" => "attachment timeout is outside the supported range",
        "daemon_startup_lock_failed" => "could not lock endpoint startup",
        "proxy_destination_missing" => "proxy host and port are required",
        "history_io_failed" => "terminal history I/O failed",
        "session_protocol_failed" => "session transport I/O or framing failed",
        "control_protocol_failed" => "control transport I/O or framing failed",
        "control_request_timeout" => "control request timed out",
        "terminal_journal_failed" => "terminal journal failed",
        "terminal_control_failed" => "terminal control failed",
        "daemon_worker_failed" => "daemon worker failed",
        "daemon_detach_failed" => "could not detach from the invoking terminal",
        "daemon_runtime_failed" => "could not initialize the async runtime",
        "ctmux_transport_failed" => "transport I/O or protocol failed",
        "pty_failed" => "PTY operation failed",
        "pane_spawn_failed" => "could not start the pane process",
        "terminal_io_failed" => "terminal I/O failed",
        "view_invalid" => "layout request was rejected",
        "input_lease_required" => "input ownership is required",
        "layout_lease_required" => "layout ownership is required",
        "lease_busy" => "another attachment owns the lease",
        "attachment_resume_rejected" => "attachment is no longer available for reconnect",
        "session_not_found" => "session was not found",
        "session_exists" => "session already exists",
        // Codes have a bounded ASCII format; no raw error text enters the log.
        _ => code,
      });
    }
    if let Some(code) = self.context.exit_code {
      let _ = write!(message, "; exit code {code}");
    }
    if let Some(code) = self.os_error {
      let _ = write!(message, "; OS error {code}");
    }
    message
  }
}
