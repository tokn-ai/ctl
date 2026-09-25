use crate::error::{CommandErrorDto, CommandResult};
use crate::keybindings::{Keybinding, valid_command_id};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommandBinding {
  pub command_id: String,
  pub title: String,
  pub keybinding: Option<Keybinding>,
  pub enabled: bool,
}

#[tauri::command]
pub async fn sync_command_menu(
  app: tauri::AppHandle,
  bindings: Vec<NativeCommandBinding>,
) -> CommandResult<()> {
  let mut ids = std::collections::HashSet::new();
  if bindings.len() > 128
    || bindings.iter().any(|binding| {
      !valid_command_id(&binding.command_id)
        || !ids.insert(&binding.command_id)
        || binding.title.is_empty()
        || binding.title.len() > 256
        || binding.title.chars().any(char::is_control)
    })
  {
    return Err(CommandErrorDto::new(
      "invalid_command_menu",
      "Invalid native command descriptors.",
    ));
  }
  // Validate on every platform, even when accelerators are handled by the webview.
  for binding in &bindings {
    if let Some(key) = &binding.keybinding {
      crate::keybindings::accelerator(key)?;
    }
  }
  #[cfg(target_os = "macos")]
  {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    app
      .run_on_main_thread(move || {
        let _ignored = sender.send(crate::native_menu::sync(&handle, &bindings));
      })
      .map_err(CommandErrorDto::backend)?;
    receiver.await.map_err(CommandErrorDto::backend)?
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = app;
    for binding in bindings {
      let _ = binding.enabled;
    }
    Ok(())
  }
}
