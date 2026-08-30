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
}
