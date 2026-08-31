use tauri::menu::{
  AboutMetadata, HELP_SUBMENU_ID, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu,
  WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Emitter as _, Manager as _};

const DETACH_TAB_COMMAND_ID: &str = "session.disconnect";
const CLOSE_SESSION_COMMAND_ID: &str = "session.close";
const NATIVE_COMMAND_EVENT: &str = "rmux://command";

pub fn build(app_handle: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
  let package = app_handle.package_info();
  let config = app_handle.config();
  let about = AboutMetadata {
    name: Some(package.name.clone()),
    version: Some(package.version.to_string()),
    copyright: config.bundle.copyright.clone(),
    authors: config
      .bundle
      .publisher
      .clone()
      .map(|publisher| vec![publisher]),
    ..Default::default()
  };

  let app_menu = Submenu::with_items(
    app_handle,
    package.name.clone(),
    true,
    &[
      &PredefinedMenuItem::about(app_handle, None, Some(about))?,
      &PredefinedMenuItem::separator(app_handle)?,
      &PredefinedMenuItem::services(app_handle, None)?,
      &PredefinedMenuItem::separator(app_handle)?,
      &PredefinedMenuItem::hide(app_handle, None)?,
      &PredefinedMenuItem::hide_others(app_handle, None)?,
      &PredefinedMenuItem::separator(app_handle)?,
      &PredefinedMenuItem::quit(app_handle, None)?,
    ],
  )?;
  let file_menu = Submenu::with_items(
    app_handle,
    "File",
    true,
    &[
      &MenuItem::with_id(
        app_handle,
        DETACH_TAB_COMMAND_ID,
        "Close Tab",
        true,
        Some("CmdOrCtrl+W"),
      )?,
      &MenuItem::with_id(
        app_handle,
        CLOSE_SESSION_COMMAND_ID,
        "Close Session…",
        true,
        Some("CmdOrCtrl+E"),
      )?,
    ],
  )?;
  let edit_menu = Submenu::with_items(
    app_handle,
    "Edit",
    true,
    &[
      &PredefinedMenuItem::undo(app_handle, None)?,
      &PredefinedMenuItem::redo(app_handle, None)?,
      &PredefinedMenuItem::separator(app_handle)?,
      &PredefinedMenuItem::cut(app_handle, None)?,
      &PredefinedMenuItem::copy(app_handle, None)?,
      &PredefinedMenuItem::paste(app_handle, None)?,
      &PredefinedMenuItem::select_all(app_handle, None)?,
    ],
  )?;
  let view_menu = Submenu::with_items(
    app_handle,
    "View",
    true,
    &[&PredefinedMenuItem::fullscreen(app_handle, None)?],
  )?;
  let window_menu = Submenu::with_id_and_items(
    app_handle,
    WINDOW_SUBMENU_ID,
    "Window",
    true,
    &[
      &PredefinedMenuItem::minimize(app_handle, None)?,
      &PredefinedMenuItem::maximize(app_handle, None)?,
    ],
  )?;
  let help_menu = Submenu::with_id_and_items(app_handle, HELP_SUBMENU_ID, "Help", true, &[])?;

  Menu::with_items(
    app_handle,
    &[
      &app_menu,
      &file_menu,
      &edit_menu,
      &view_menu,
      &window_menu,
      &help_menu,
    ],
  )
}

pub fn handle(app_handle: &AppHandle, event: &tauri::menu::MenuEvent) {
  let Some(command_id) = frontend_command(event.id()) else {
    return;
  };
  let target = app_handle
    .webview_windows()
    .into_values()
    .find(|window| window.is_focused().unwrap_or(false))
    .or_else(|| app_handle.get_webview_window("main"));
  if let Some(window) = target {
    let _ignored = window.emit(NATIVE_COMMAND_EVENT, command_id);
  }
}

fn frontend_command(menu_id: &MenuId) -> Option<&'static str> {
  if menu_id == DETACH_TAB_COMMAND_ID {
    Some(DETACH_TAB_COMMAND_ID)
  } else if menu_id == CLOSE_SESSION_COMMAND_ID {
    Some(CLOSE_SESSION_COMMAND_ID)
  } else {
    None
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn maps_only_rmux_session_menu_items_to_frontend_commands() {
    assert_eq!(
      frontend_command(&MenuId::new(DETACH_TAB_COMMAND_ID)),
      Some(DETACH_TAB_COMMAND_ID)
    );
    assert_eq!(
      frontend_command(&MenuId::new(CLOSE_SESSION_COMMAND_ID)),
      Some(CLOSE_SESSION_COMMAND_ID)
    );
    assert_eq!(frontend_command(&MenuId::new("unrelated")), None);
  }
}
