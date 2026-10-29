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
  validate(&bindings)?;
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

fn validate(bindings: &[NativeCommandBinding]) -> CommandResult<()> {
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
  for binding in bindings {
    if let Some(key) = &binding.keybinding {
      crate::keybindings::accelerator(key)?;
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn binding(command_id: &str) -> NativeCommandBinding {
    NativeCommandBinding {
      command_id: command_id.into(),
      title: command_id.into(),
      keybinding: None,
      enabled: true,
    }
  }

  #[test]
  fn accepts_the_complete_frontend_command_registry_with_native_shortcuts() {
    // The TypeScript test checks this fixture against CONFIGURABLE_COMMAND_IDS.
    // One invalid ID otherwise rejects the entire native shortcut menu.
    let ids: Vec<String> = serde_json::from_str(include_str!(
      "../../src/features/commands/commandIds.contract.json"
    ))
    .unwrap();
    let bindings: Vec<_> = ids
      .iter()
      .map(|id| {
        let mut descriptor = binding(id);
        descriptor.keybinding = match id.as_str() {
          "view.show_command_palette" => Some(Keybinding {
            code: "KeyP".into(),
            primary: true,
            shift: true,
            alt: false,
          }),
          "tab.new_shell_here" => Some(Keybinding {
            code: "KeyT".into(),
            primary: true,
            shift: false,
            alt: false,
          }),
          _ => None,
        };
        descriptor
      })
      .collect();
    validate(&bindings).unwrap();
  }

  #[test]
  fn rejects_invalid_descriptors_before_replacing_the_menu() {
    assert!(validate(&[binding("session.close"), binding("session.close")]).is_err());
    assert!(validate(&[binding("session.close\n")]).is_err());
    for title in [String::new(), "x".repeat(257), "New\nShell".into()] {
      let mut descriptor = binding("session.new_shell");
      descriptor.title = title;
      assert!(validate(&[descriptor]).is_err());
    }
    let mut descriptor = binding("session.new_shell");
    descriptor.keybinding = Some(Keybinding {
      code: "unsupported".into(),
      primary: true,
      shift: false,
      alt: false,
    });
    assert!(validate(&[descriptor]).is_err());
  }
}
