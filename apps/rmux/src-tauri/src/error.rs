use ctl_core::CoreError;
use rmux_client::ClientError;
use rmux_proto::ErrorCode;
use serde::Serialize;

pub type CommandResult<T> = Result<T, CommandErrorDto>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandErrorDto {
  pub code: String,
  pub message: String,
}

impl CommandErrorDto {
  pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      code: code.into(),
      message: message.into(),
    }
  }

  pub fn backend(error: impl std::fmt::Display) -> Self {
    Self::new("backend_error", error.to_string())
  }

  pub fn client(error: ClientError) -> Self {
    match error {
      ClientError::Server { code, message } => Self::new(protocol_error_code(&code), message),
      error => Self::backend(error),
    }
  }

  pub fn transport(error: &CoreError) -> Self {
    match error {
      CoreError::InvalidSshDestination(_) | CoreError::InvalidSshOption(_) => {
        Self::new("invalid_ssh_destination", error.to_string())
      }
      CoreError::StartSsh(_) | CoreError::MissingSshStdin | CoreError::MissingSshStdout => {
        Self::new("ssh_start_failed", error.to_string())
      }
      CoreError::ReadSshPreface(_) => Self::new("ssh_connection_failed", error.to_string()),
      CoreError::SshStartup(message) => {
        let lower = message.to_lowercase();
        let code = if lower.contains("host key verification")
          || lower.contains("host identification has changed")
        {
          "ssh_host_key_failed"
        } else if lower.contains("permission denied") {
          "ssh_authentication_failed"
        } else if lower.contains("ctld")
          && (lower.contains("not found") || lower.contains("no such file"))
        {
          "ctld_not_found"
        } else {
          "ssh_connection_failed"
        };
        Self::new(code, error.to_string())
      }
      CoreError::InvalidSshPreface => Self::new("invalid_ssh_preface", error.to_string()),
      #[cfg(unix)]
      CoreError::LocalIpc(_) => Self::new("local_connection_failed", error.to_string()),
      #[cfg(not(unix))]
      CoreError::LocalTransportUnsupported => Self::new("unsupported_platform", error.to_string()),
    }
  }
}

pub fn protocol_error_code(code: &ErrorCode) -> &'static str {
  match code {
    ErrorCode::InvalidRequest => "invalid_request",
    ErrorCode::InvalidSessionName => "invalid_session_name",
    ErrorCode::ProtocolVersionMismatch => "protocol_version_mismatch",
    ErrorCode::SequenceAhead => "sequence_ahead",
    ErrorCode::SessionAlreadyExists => "session_already_exists",
    ErrorCode::SessionNotFound => "session_not_found",
    ErrorCode::AttachmentResumeRejected => "attachment_resume_rejected",
    ErrorCode::InputLeaseRequired => "input_lease_required",
    ErrorCode::LayoutLeaseRequired => "layout_lease_required",
    ErrorCode::Internal => "internal",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn client_server_errors_preserve_the_protocol_code() {
    let error = CommandErrorDto::client(ClientError::Server {
      code: ErrorCode::SessionNotFound,
      message: "already gone".into(),
    });

    assert_eq!(error.code, "session_not_found");
    assert_eq!(error.message, "already gone");
  }
}
