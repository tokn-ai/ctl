use std::collections::HashMap;

use crate::error::{CommandErrorDto, CommandResult};
use crate::keybindings::{Keybinding, valid_command_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCommandBinding {
  pub command_id: String,
  pub title: String,
  pub keybinding: Option<Keybinding>,
  pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct RejectedNativeCommand {
  pub command_id: Option<String>,
  pub index: usize,
  pub message: String,
}

#[derive(Debug, Default, Serialize)]
pub struct NativeCommandMenuResult {
  pub rejected: Vec<RejectedNativeCommand>,
}

#[derive(Debug)]
pub struct PreparedNativeCommand {
  pub binding: NativeCommandBinding,
  pub accelerator: Option<String>,
  index: usize,
}

#[derive(Debug)]
pub struct PreparedCommandMenu {
  commands: Vec<PreparedNativeCommand>,
  rejected: Vec<RejectedNativeCommand>,
}

impl PreparedCommandMenu {
  /// A descriptor or native item failure affects only that command. The caller
  /// still installs the complete replacement menu to remove obsolete shortcuts.
  pub fn install(
    mut self,
    mut install: impl FnMut(&PreparedNativeCommand) -> CommandResult<()>,
  ) -> NativeCommandMenuResult {
    for command in self.commands {
      if let Err(failure) = install(&command) {
        self.rejected.push(RejectedNativeCommand {
          command_id: Some(command.binding.command_id),
          index: command.index,
          message: failure.message,
        });
      }
    }
    self.rejected.sort_by_key(|rejection| rejection.index);
    NativeCommandMenuResult {
      rejected: self.rejected,
    }
  }
}

#[tauri::command]
pub async fn sync_command_menu(
  app: tauri::AppHandle,
  bindings: Vec<Value>,
) -> CommandResult<NativeCommandMenuResult> {
  // Decode entries individually: even a malformed descriptor must not prevent
  // unrelated shortcuts from reaching the native menu.
  let prepared = prepare(bindings)?;
  #[cfg(target_os = "macos")]
  {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    app
      .run_on_main_thread(move || {
        let _ignored = sender.send(crate::native_menu::sync(&handle, prepared));
      })
      .map_err(CommandErrorDto::backend)?;
    receiver.await.map_err(CommandErrorDto::backend)?
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = app;
    Ok(prepared.install(|_| Ok(())))
  }
}

fn prepare(bindings: Vec<Value>) -> CommandResult<PreparedCommandMenu> {
  if bindings.len() > 128 {
    return Err(CommandErrorDto::new(
      "invalid_command_menu",
      "Too many native command descriptors.",
    ));
  }

  let mut id_counts = HashMap::new();
  for value in &bindings {
    if let Some(id) = value.get("command_id").and_then(Value::as_str) {
      *id_counts.entry(id.to_owned()).or_insert(0) += 1;
    }
  }
  let mut commands = Vec::new();
  let mut rejected = Vec::new();
  for (index, value) in bindings.into_iter().enumerate() {
    let command_id = value
      .get("command_id")
      .and_then(Value::as_str)
      .filter(|id| id.len() <= 128 && !id.chars().any(char::is_control))
      .map(str::to_owned);
    let decoded = serde_json::from_value::<NativeCommandBinding>(value)
      .map_err(|failure| format!("Invalid command descriptor: {failure}"))
      .and_then(|binding| {
        if !valid_command_id(&binding.command_id) {
          return Err("Invalid command ID.".into());
        }
        if id_counts
          .get(&binding.command_id)
          .is_some_and(|count| *count > 1)
        {
          return Err("Duplicate command ID.".into());
        }
        if binding.title.is_empty()
          || binding.title.len() > 256
          || binding.title.chars().any(char::is_control)
        {
          return Err("Invalid command title.".into());
        }
        let accelerator = binding
          .keybinding
          .as_ref()
          .map(crate::keybindings::accelerator)
          .transpose()
          .map_err(|failure| failure.message)?;
        Ok(PreparedNativeCommand {
          // Disabled commands must not retain a previously active accelerator.
          accelerator: if binding.enabled { accelerator } else { None },
          binding,
          index,
        })
      });
    match decoded {
      Ok(command) => commands.push(command),
      Err(message) => rejected.push(RejectedNativeCommand {
        command_id,
        index,
        message,
      }),
    }
  }

  let mut accelerator_counts = HashMap::new();
  for command in &commands {
    if let Some(accelerator) = &command.accelerator {
      *accelerator_counts.entry(accelerator.clone()).or_insert(0) += 1;
    }
  }
  commands.retain(|command| {
    if command
      .accelerator
      .as_ref()
      .is_some_and(|key| accelerator_counts[key] > 1)
    {
      rejected.push(RejectedNativeCommand {
        command_id: Some(command.binding.command_id.clone()),
        index: command.index,
        message: "Shortcut is assigned to more than one command.".into(),
      });
      false
    } else {
      true
    }
  });
  Ok(PreparedCommandMenu { commands, rejected })
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn binding(command_id: &str) -> Value {
    json!({ "command_id": command_id, "title": command_id, "keybinding": null, "enabled": true })
  }

  fn shortcut(command_id: &str, code: &str, shift: bool) -> Value {
    let mut value = binding(command_id);
    value["keybinding"] = json!({ "code": code, "primary": true, "shift": shift });
    value
  }

  fn healthy_shortcuts() -> Vec<Value> {
    vec![
      shortcut("view.show_command_palette", "KeyP", true),
      shortcut("tab.new_shell_here", "KeyT", false),
    ]
  }

  #[test]
  fn accepts_the_complete_frontend_command_registry_with_native_shortcuts() {
    // The TypeScript test checks this fixture against CONFIGURABLE_COMMAND_IDS.
    let ids: Vec<String> = serde_json::from_str(include_str!(
      "../../src/features/commands/commandIds.contract.json"
    ))
    .unwrap();
    let bindings = ids
      .iter()
      .map(|id| match id.as_str() {
        "view.show_command_palette" => shortcut(id, "KeyP", true),
        "tab.new_shell_here" => shortcut(id, "KeyT", false),
        _ => binding(id),
      })
      .collect();
    let mut installed = Vec::new();
    let report = prepare(bindings).unwrap().install(|command| {
      installed.push(command.binding.command_id.clone());
      Ok(())
    });
    assert_eq!(installed, ids);
    assert_eq!(report.rejected.len(), 0);
  }

  #[test]
  fn malformed_commands_do_not_block_palette_or_new_tab_shortcuts() {
    let mut bad_title = binding("task.restart");
    bad_title["title"] = json!("New\nShell");
    let mut bad_type = binding("task.restart");
    bad_type["enabled"] = json!("yes");
    let mut unknown_field = binding("task.restart");
    unknown_field["extra"] = json!(true);
    for invalid in [
      binding("task/restart"),
      bad_title,
      bad_type,
      unknown_field,
      json!(null),
      shortcut("task.restart", "unsupported", false),
    ] {
      let mut bindings = healthy_shortcuts();
      bindings.insert(1, invalid);
      let mut installed = Vec::new();
      let report = prepare(bindings).unwrap().install(|command| {
        installed.push((
          command.binding.command_id.clone(),
          command.accelerator.clone(),
        ));
        Ok(())
      });
      assert_eq!(
        installed,
        vec![
          (
            "view.show_command_palette".into(),
            Some("CmdOrCtrl+Shift+P".into())
          ),
          ("tab.new_shell_here".into(), Some("CmdOrCtrl+T".into())),
        ]
      );
      assert_eq!(report.rejected.len(), 1);
      assert_eq!(report.rejected[0].index, 1);
    }
  }

  #[test]
  fn conflicting_ids_or_shortcuts_reject_only_the_ambiguous_commands() {
    for conflicts in [
      vec![binding("task.restart"), binding("task.restart")],
      vec![
        shortcut("task.restart", "KeyR", false),
        shortcut("task.refresh", "KeyR", false),
      ],
    ] {
      let mut bindings = healthy_shortcuts();
      bindings.splice(1..1, conflicts);
      let mut installed = Vec::new();
      let report = prepare(bindings).unwrap().install(|command| {
        installed.push(command.binding.command_id.clone());
        Ok(())
      });
      assert_eq!(
        installed,
        ["view.show_command_palette", "tab.new_shell_here"]
      );
      assert_eq!(
        report
          .rejected
          .iter()
          .map(|entry| entry.index)
          .collect::<Vec<_>>(),
        [1, 2]
      );
    }
  }

  #[test]
  fn native_item_failure_does_not_stop_installing_other_commands() {
    let mut bindings = healthy_shortcuts();
    bindings.insert(1, binding("ctl-taskd.restart"));
    let mut installed = Vec::new();
    let report = prepare(bindings).unwrap().install(|command| {
      if command.binding.command_id == "ctl-taskd.restart" {
        return Err(CommandErrorDto::backend(
          "Native item could not be created.",
        ));
      }
      installed.push(command.binding.command_id.clone());
      Ok(())
    });
    assert_eq!(
      installed,
      ["view.show_command_palette", "tab.new_shell_here"]
    );
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(
      report.rejected[0].command_id.as_deref(),
      Some("ctl-taskd.restart")
    );
    assert_eq!(
      report.rejected[0].message,
      "Native item could not be created."
    );
  }

  #[test]
  fn a_disabled_command_has_no_accelerator_or_conflict() {
    let mut bindings = healthy_shortcuts();
    let mut disabled = shortcut("task.restart", "KeyT", false);
    disabled["enabled"] = json!(false);
    bindings.push(disabled);
    let mut installed = Vec::new();
    let report = prepare(bindings).unwrap().install(|command| {
      installed.push((command.binding.enabled, command.accelerator.clone()));
      Ok(())
    });
    assert_eq!(installed.len(), 3);
    assert_eq!(installed[2], (false, None));
    assert_eq!(report.rejected.len(), 0);
  }

  #[test]
  fn enforces_the_request_limit_before_attempting_any_installation() {
    let failure = prepare(vec![binding("task.restart"); 129]).unwrap_err();
    assert_eq!(failure.code, "invalid_command_menu");
    let report = prepare(Vec::new())
      .unwrap()
      .install(|_| panic!("no commands"));
    assert_eq!(report.rejected.len(), 0);
  }
}
