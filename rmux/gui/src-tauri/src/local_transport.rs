use crate::error::CommandErrorDto;

#[cfg(unix)]
pub type LocalStream = tokio::net::UnixStream;

#[cfg(not(unix))]
pub type LocalStream = tokio::io::DuplexStream;

#[cfg(unix)]
pub async fn connect() -> Result<LocalStream, CommandErrorDto> {
  rmux_ipc::connect_or_start_daemon(&rmux_ipc::socket_path())
    .await
    .map_err(CommandErrorDto::backend)
}

pub fn default_working_directory() -> Result<String, CommandErrorDto> {
  #[cfg(unix)]
  const HOME_ENVIRONMENT: &str = "HOME";
  #[cfg(windows)]
  const HOME_ENVIRONMENT: &str = "USERPROFILE";
  #[cfg(not(any(unix, windows)))]
  const HOME_ENVIRONMENT: &str = "HOME";

  let directory = std::env::var_os(HOME_ENVIRONMENT).ok_or_else(|| {
    CommandErrorDto::new(
      "home_directory_unavailable",
      "enter a working directory because the user home directory is unavailable",
    )
  })?;
  directory.into_string().map_err(|_directory| {
    CommandErrorDto::new(
      "home_directory_not_utf8",
      "enter a working directory because the user home directory is not valid UTF-8",
    )
  })
}

#[cfg(not(unix))]
pub async fn connect() -> Result<LocalStream, CommandErrorDto> {
  Err(CommandErrorDto::new(
    "unsupported_platform",
    "local rmux transport is not implemented on this platform",
  ))
}
