use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEventType};
use tauri::menu::{
  AboutMetadata, HELP_SUBMENU_ID, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu,
  WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Emitter as _, Manager as _};

const NATIVE_COMMAND_EVENT: &str = "rmux://command";
const COMMAND_MENU_ID: &str = "rmux.commands";

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
  let file_menu = Submenu::with_id_and_items(app_handle, COMMAND_MENU_ID, "Commands", true, &[])?;
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
  // Native menu accelerators bypass the webview's KeyboardEvent.repeat guard.
  // Check AppKit on the event-loop thread before emitting the command to JS.
  if current_event_is_key_repeat() {
    return;
  }
  let target = app_handle
    .webview_windows()
    .into_values()
    .find(|window| window.is_focused().unwrap_or(false))
    .or_else(|| app_handle.get_webview_window("main"));
  if let Some(window) = target {
    let _ignored = window.emit(NATIVE_COMMAND_EVENT, command_id);
  }
}

pub fn sync(
  app: &AppHandle,
  bindings: &[crate::command_menu::NativeCommandBinding],
) -> crate::error::CommandResult<()> {
  use crate::error::CommandErrorDto;
  let menu = app.menu().ok_or_else(|| {
    CommandErrorDto::new(
      "command_menu_missing",
      "The application menu is unavailable.",
    )
  })?;
  let next = Submenu::with_id_and_items(app, COMMAND_MENU_ID, "Commands", true, &[])
    .map_err(CommandErrorDto::backend)?;
  for binding in bindings {
    let accelerator = binding
      .keybinding
      .as_ref()
      .map(crate::keybindings::accelerator)
      .transpose()?;
    let item = MenuItem::with_id(
      app,
      format!("rmux.command.{}", binding.command_id),
      &binding.title,
      binding.enabled,
      accelerator.as_deref(),
    )
    .map_err(CommandErrorDto::backend)?;
    next.append(&item).map_err(CommandErrorDto::backend)?;
  }
  let previous = menu.get(COMMAND_MENU_ID);
  if let Some(previous) = &previous {
    menu.remove(previous).map_err(CommandErrorDto::backend)?;
  }
  if let Err(failure) = menu.insert(&next, 1) {
    if let Some(previous) = previous {
      let _ignored = menu.insert(&previous, 1);
    }
    return Err(CommandErrorDto::backend(failure));
  }
  Ok(())
}

fn current_event_is_key_repeat() -> bool {
  let Some(main_thread) = MainThreadMarker::new() else {
    return false;
  };
  let Some(event) = NSApplication::sharedApplication(main_thread).currentEvent() else {
    return false;
  };
  is_key_repeat(event.r#type(), || event.isARepeat())
}

fn frontend_command(menu_id: &MenuId) -> Option<&str> {
  menu_id
    .as_ref()
    .strip_prefix("rmux.command.")
    .filter(|id| crate::keybindings::valid_command_id(id))
}

fn is_key_repeat(event_type: NSEventType, is_repeat: impl FnOnce() -> bool) -> bool {
  // isARepeat raises an AppKit exception for mouse and modifier-change events.
  event_type == NSEventType::KeyDown && is_repeat()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ignores_held_shortcuts_but_accepts_distinct_key_presses() {
    assert!(is_key_repeat(NSEventType::KeyDown, || true));
    assert!(!is_key_repeat(NSEventType::KeyDown, || false));
  }

  #[test]
  fn does_not_query_repeat_state_for_mouse_menu_actions_or_modifier_events() {
    for event_type in [
      NSEventType::LeftMouseUp,
      NSEventType::FlagsChanged,
      NSEventType::ApplicationDefined,
    ] {
      assert!(!is_key_repeat(event_type, || panic!("not a key event")));
    }
  }

  #[test]
  fn maps_only_rmux_session_menu_items_to_frontend_commands() {
    assert_eq!(
      frontend_command(&MenuId::new("rmux.command.session.close")),
      Some("session.close")
    );
    assert!(frontend_command(&MenuId::new("undo")).is_none());
    assert!(frontend_command(&MenuId::new("rmux.command.")).is_none());
  }
}
