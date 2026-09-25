use serde::{Deserialize, Serialize};

use crate::error::{CommandErrorDto, CommandResult};

#[derive(Deserialize)]
pub struct CheckLocalPortRequest {
  port: u16,
}

#[derive(Serialize)]
pub struct LocalPortAvailability {
  port: u16,
  available: bool,
  message: Option<String>,
}

#[tauri::command]
pub async fn check_local_port(
  request: CheckLocalPortRequest,
) -> CommandResult<LocalPortAvailability> {
  if request.port == 0 {
    return Err(CommandErrorDto::new(
      "invalid_local_port",
      "Local ports must be between 1 and 65535.",
    ));
  }
  match tokio::net::TcpListener::bind(("127.0.0.1", request.port)).await {
    Ok(listener) => {
      drop(listener);
      Ok(LocalPortAvailability {
        port: request.port,
        available: true,
        message: None,
      })
    }
    Err(error) => Ok(LocalPortAvailability {
      port: request.port,
      available: false,
      message: Some(error.to_string()),
    }),
  }
}
