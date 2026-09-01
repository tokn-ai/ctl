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
