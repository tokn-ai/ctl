use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEventType};
use std::sync::Mutex;
use tauri::menu::{
  HELP_SUBMENU_ID, Menu, MenuId, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu,
  WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Emitter as _, Manager as _};

use crate::command_menu::NativeMenuLabels;

const NATIVE_COMMAND_EVENT: &str = "ctmux://command";
const COMMAND_MENU_ID: &str = "ctmux.commands";

type LabelGetter = fn(&NativeMenuLabels) -> &str;

struct StaticMenuItems {
  items: Vec<(MenuItemKind<tauri::Wry>, LabelGetter)>,
  applied: Mutex<Option<NativeMenuLabels>>,
}

impl StaticMenuItems {
  fn localize(&self, labels: NativeMenuLabels) -> tauri::Result<()> {
    let mut applied = self.applied.lock().expect("native menu labels");
    if applied.as_ref() == Some(&labels) {
      return Ok(());
    }
    for (item, text) in &self.items {
      let label = text(&labels);
      match item {
        MenuItemKind::MenuItem(item) => item.set_text(label)?,
        MenuItemKind::Submenu(item) => item.set_text(label)?,
        MenuItemKind::Predefined(item) => item.set_text(label)?,
        _ => unreachable!("only registered static menu items are localized"),
      }
    }
    *applied = Some(labels);
    Ok(())
  }
}

pub fn build(app_handle: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
  let package = app_handle.package_info();
  let about = MenuItem::with_id(
    app_handle,
    "ctmux.command.app.about",
    "About ctmux",
    true,
    None::<&str>,
  )?;
  let services = PredefinedMenuItem::services(app_handle, None)?;
  let hide = PredefinedMenuItem::hide(app_handle, None)?;
  let hide_others = PredefinedMenuItem::hide_others(app_handle, None)?;
  let quit = PredefinedMenuItem::quit(app_handle, None)?;
  let app_menu = Submenu::with_items(
    app_handle,
    package.name.clone(),
    true,
    &[
      &about,
      &PredefinedMenuItem::separator(app_handle)?,
      &services,
      &PredefinedMenuItem::separator(app_handle)?,
      &hide,
      &hide_others,
      &PredefinedMenuItem::separator(app_handle)?,
      &quit,
    ],
  )?;
  let file_menu = Submenu::with_id_and_items(app_handle, COMMAND_MENU_ID, "Commands", true, &[])?;
  let undo = PredefinedMenuItem::undo(app_handle, None)?;
  let redo = PredefinedMenuItem::redo(app_handle, None)?;
  let cut = PredefinedMenuItem::cut(app_handle, None)?;
  let copy = PredefinedMenuItem::copy(app_handle, None)?;
  let paste = PredefinedMenuItem::paste(app_handle, None)?;
  let select_all = PredefinedMenuItem::select_all(app_handle, None)?;
  let edit_menu = Submenu::with_items(
    app_handle,
    "Edit",
    true,
    &[
      &undo,
      &redo,
      &PredefinedMenuItem::separator(app_handle)?,
      &cut,
      &copy,
      &paste,
      &select_all,
    ],
  )?;
  let fullscreen = PredefinedMenuItem::fullscreen(app_handle, None)?;
  let view_menu = Submenu::with_items(app_handle, "View", true, &[&fullscreen])?;
  let minimize = PredefinedMenuItem::minimize(app_handle, None)?;
  let maximize = PredefinedMenuItem::maximize(app_handle, None)?;
  let window_menu = Submenu::with_id_and_items(
    app_handle,
    WINDOW_SUBMENU_ID,
    "Window",
    true,
    &[&minimize, &maximize],
  )?;
  let help_menu = Submenu::with_id_and_items(app_handle, HELP_SUBMENU_ID, "Help", true, &[])?;

  let menu = Menu::with_items(
    app_handle,
    &[
      &app_menu,
      &file_menu,
      &edit_menu,
      &view_menu,
      &window_menu,
      &help_menu,
    ],
  )?;
  app_handle.manage(StaticMenuItems {
    items: vec![
      (MenuItemKind::MenuItem(about), |labels| &labels.about),
      (MenuItemKind::Submenu(edit_menu), |labels| &labels.edit),
      (MenuItemKind::Submenu(view_menu), |labels| &labels.view),
      (MenuItemKind::Submenu(window_menu), |labels| &labels.window),
      (MenuItemKind::Submenu(help_menu), |labels| &labels.help),
      (MenuItemKind::Predefined(services), |labels| {
        &labels.services
      }),
      (MenuItemKind::Predefined(hide), |labels| &labels.hide),
      (MenuItemKind::Predefined(hide_others), |labels| {
        &labels.hide_others
      }),
      (MenuItemKind::Predefined(quit), |labels| &labels.quit),
      (MenuItemKind::Predefined(undo), |labels| &labels.undo),
      (MenuItemKind::Predefined(redo), |labels| &labels.redo),
      (MenuItemKind::Predefined(cut), |labels| &labels.cut),
      (MenuItemKind::Predefined(copy), |labels| &labels.copy),
      (MenuItemKind::Predefined(paste), |labels| &labels.paste),
      (MenuItemKind::Predefined(select_all), |labels| {
        &labels.select_all
      }),
      (MenuItemKind::Predefined(fullscreen), |labels| {
        &labels.fullscreen
      }),
      (MenuItemKind::Predefined(minimize), |labels| {
        &labels.minimize
      }),
      (MenuItemKind::Predefined(maximize), |labels| {
        &labels.maximize
      }),
    ],
    applied: Mutex::default(),
  });
  Ok(menu)
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
    if window.label().starts_with("preview-") {
      if command_id == "session.disconnect" {
        let _ignored = window.close();
      }
      return;
    }
    let _ignored = window.emit(NATIVE_COMMAND_EVENT, command_id);
  }
}

pub fn sync(
  app: &AppHandle,
  prepared: crate::command_menu::PreparedCommandMenu,
  labels: Option<NativeMenuLabels>,
) -> crate::error::CommandResult<crate::command_menu::NativeCommandMenuResult> {
  use crate::error::CommandErrorDto;
  let menu = app.menu().ok_or_else(|| {
    CommandErrorDto::new(
      "command_menu_missing",
      "The application menu is unavailable.",
    )
  })?;
  let previous = menu.get(COMMAND_MENU_ID);
  let command_title = labels
    .as_ref()
    .map(|labels| labels.commands.clone())
    .or_else(|| {
      previous
        .as_ref()
        .and_then(MenuItemKind::as_submenu)
        .and_then(|item| item.text().ok())
    });
  let next = Submenu::with_id_and_items(
    app,
    COMMAND_MENU_ID,
    command_title.as_deref().unwrap_or("Commands"),
    true,
    &[],
  )
  .map_err(CommandErrorDto::backend)?;
  let report = prepared.install(|command| {
    let binding = &command.binding;
    let item = MenuItem::with_id(
      app,
      format!("ctmux.command.{}", binding.command_id),
      &binding.title,
      binding.enabled,
      command.accelerator.as_deref(),
    )
    .map_err(CommandErrorDto::backend)?;
    next.append(&item).map_err(CommandErrorDto::backend)
  });
  if let Some(labels) = labels {
    app
      .state::<StaticMenuItems>()
      .localize(labels)
      .map_err(CommandErrorDto::backend)?;
  }
  if let Some(previous) = &previous {
    menu.remove(previous).map_err(CommandErrorDto::backend)?;
  }
  if let Err(failure) = menu.insert(&next, 1) {
    if let Some(previous) = previous {
      let _ignored = menu.insert(&previous, 1);
    }
    return Err(CommandErrorDto::backend(failure));
  }
  Ok(report)
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
    .strip_prefix("ctmux.command.")
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
  fn maps_only_ctmux_session_menu_items_to_frontend_commands() {
    assert_eq!(
      frontend_command(&MenuId::new("ctmux.command.app.about")),
      Some("app.about")
    );
    assert_eq!(
      frontend_command(&MenuId::new("ctmux.command.session.close")),
      Some("session.close")
    );
    assert_eq!(
      frontend_command(&MenuId::new("ctmux.command.ctl-taskd.restart")),
      Some("ctl-taskd.restart")
    );
    assert!(frontend_command(&MenuId::new("undo")).is_none());
    assert!(frontend_command(&MenuId::new("ctmux.command.")).is_none());
  }
}
