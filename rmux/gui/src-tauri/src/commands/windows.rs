use tauri::{AppHandle, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::dto::WindowBootstrapDto;
use crate::error::{CommandErrorDto, CommandResult};
use crate::state::{AppState, register_window_cleanup};

const WINDOW_WIDTH: f64 = 1120.0;
const WINDOW_HEIGHT: f64 = 720.0;
const WINDOW_MIN_WIDTH: f64 = 720.0;
const WINDOW_MIN_HEIGHT: f64 = 480.0;

#[tauri::command]
pub async fn open_shell_window(
  app: AppHandle,
  state: State<'_, AppState>,
  request: WindowBootstrapDto,
) -> CommandResult<()> {
  if request.working_directory.is_empty() {
    return Err(CommandErrorDto::new(
      "working_directory_unavailable",
      "the current session did not report a working directory",
    ));
  }
  request.terminal_size.clone().into_proto()?;

  let window_label = format!("terminal-{}", uuid::Uuid::new_v4());
  state.store_window_bootstrap(&window_label, request).await?;

  let window = build_shell_window(&app, &window_label);
  let window = match window {
    Ok(window) => window,
    Err(error) => {
      state.remove_window_bootstrap(&window_label).await;
      return Err(CommandErrorDto::backend(error));
    }
  };
  register_window_cleanup(&window, state.inner().clone());
  Ok(())
}

#[tauri::command]
pub async fn take_window_bootstrap(
  window: WebviewWindow,
  state: State<'_, AppState>,
) -> CommandResult<Option<WindowBootstrapDto>> {
  Ok(state.take_window_bootstrap(window.label()).await)
}

fn build_shell_window(app: &AppHandle, window_label: &str) -> tauri::Result<WebviewWindow> {
  WebviewWindowBuilder::new(app, window_label, WebviewUrl::App("index.html".into()))
    .title("rmux")
    .inner_size(WINDOW_WIDTH, WINDOW_HEIGHT)
    .min_inner_size(WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT)
    .focused(true)
    .build()
}
