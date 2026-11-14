//! Preview-only windows receive an in-memory snapshot through an opaque ID.
#![allow(clippy::needless_pass_by_value)]

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

const MAX_PREVIEW_WINDOWS: usize = 16;
const MAX_CONTENT_BYTES: usize = 6 * 1024 * 1024;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
  Text,
  Markdown,
  Image,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewSnapshot {
  target: ConnectionTargetDto,
  path: String,
  line: Option<u32>,
  column: Option<u32>,
  kind: PreviewKind,
  content: String,
  scroll_top: f64,
  title: String,
  #[serde(default)]
  truncated: bool,
}

impl PreviewSnapshot {
  fn validate(&self) -> CommandResult<()> {
    if self.content.len() > MAX_CONTENT_BYTES
      || self.path.len() > 32 * 1024
      || self.path.contains('\0')
      || self.title.len() > 1024
      || self.title.chars().any(char::is_control)
      || !self.scroll_top.is_finite()
      || self.scroll_top < 0.0
      || self.line == Some(0)
      || self.column == Some(0)
    {
      return Err(CommandErrorDto::new(
        "invalid_preview",
        "The preview exceeds its supported bounds.",
      ));
    }
    if matches!(self.kind, PreviewKind::Image) {
      let valid = self
        .content
        .split_once(";base64,")
        .is_some_and(|(media_type, data)| {
          matches!(
            media_type,
            "data:image/png"
              | "data:image/jpeg"
              | "data:image/gif"
              | "data:image/webp"
              | "data:image/bmp"
              | "data:image/avif"
              | "data:image/x-icon"
          ) && base64::engine::general_purpose::STANDARD
            .decode(data)
            .is_ok_and(|bytes| bytes.len() <= 4 * 1024 * 1024)
        });
      if !valid {
        return Err(CommandErrorDto::new(
          "invalid_preview",
          "The image preview is not a supported bounded image.",
        ));
      }
    }
    Ok(())
  }
}

#[derive(Default)]
pub struct PreviewWindows(Mutex<HashMap<String, PreviewSnapshot>>);

impl PreviewWindows {
  pub fn close_window(&self, label: &str) {
    self.0.lock().unwrap().remove(label);
  }

  fn reserve(&self, snapshot: PreviewSnapshot) -> CommandResult<(String, String)> {
    snapshot.validate()?;
    let mut windows = self.0.lock().unwrap();
    if windows.len() >= MAX_PREVIEW_WINDOWS {
      return Err(CommandErrorDto::new(
        "preview_capacity",
        "Close a preview window before opening another.",
      ));
    }
    let preview_id = uuid::Uuid::new_v4().to_string();
    let label = format!("preview-{preview_id}");
    windows.insert(label.clone(), snapshot);
    Ok((preview_id, label))
  }

  fn load(&self, label: &str) -> CommandResult<PreviewSnapshot> {
    self.0.lock().unwrap().get(label).cloned().ok_or_else(|| {
      CommandErrorDto::new("preview_unavailable", "This window has no file preview.")
    })
  }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenPreviewRequest {
  snapshot: PreviewSnapshot,
  window_title: Option<String>,
}

#[tauri::command]
pub async fn open_preview_window(
  app: AppHandle,
  state: State<'_, PreviewWindows>,
  request: OpenPreviewRequest,
) -> CommandResult<String> {
  let title = request
    .window_title
    .filter(|title| {
      !title.is_empty() && title.len() <= 2048 && !title.chars().any(char::is_control)
    })
    .unwrap_or_else(|| format!("{} — Preview", request.snapshot.title));
  let (preview_id, label) = state.reserve(request.snapshot)?;
  // The URL contains no file path, content, SSH target, or authentication data.
  let result = WebviewWindowBuilder::new(
    &app,
    &label,
    WebviewUrl::App(format!("index.html?preview_id={preview_id}").into()),
  )
  .title(title)
  .inner_size(900.0, 680.0)
  .min_inner_size(360.0, 260.0)
  .build();
  if let Err(error) = result {
    state.close_window(&label);
    return Err(CommandErrorDto::backend(error));
  }
  Ok(preview_id)
}

#[tauri::command]
pub fn load_preview_window(
  window: WebviewWindow,
  state: State<'_, PreviewWindows>,
) -> CommandResult<PreviewSnapshot> {
  // An arbitrary renderer-supplied ID cannot read another window's snapshot.
  state.load(window.label())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn snapshot() -> PreviewSnapshot {
    PreviewSnapshot {
      target: ConnectionTargetDto::Local,
      path: "/project/readme.md".into(),
      line: Some(12),
      column: Some(3),
      kind: PreviewKind::Markdown,
      content: "# Preview".into(),
      scroll_top: 124.0,
      title: "readme.md".into(),
      truncated: true,
    }
  }

  #[test]
  fn promotion_retains_state_and_is_owned_by_the_dedicated_window() {
    let registry = PreviewWindows::default();
    let (id, label) = registry.reserve(snapshot()).unwrap();
    assert!(uuid::Uuid::parse_str(&id).is_ok());
    assert!(registry.load("main").is_err());
    let restored = registry.load(&label).unwrap();
    assert_eq!(restored.line, Some(12));
    assert_eq!(restored.scroll_top, 124.0);
    assert!(restored.truncated);
    registry.close_window("main");
    assert!(registry.load(&label).is_ok());
    registry.close_window(&label);
    assert!(registry.load(&label).is_err());
  }

  #[test]
  fn rejects_executable_images_and_unbounded_state() {
    let mut preview = snapshot();
    preview.kind = PreviewKind::Image;
    preview.content = "data:image/svg+xml;base64,PHN2Zy8+".into();
    assert!(preview.validate().is_err());
    preview.kind = PreviewKind::Text;
    preview.scroll_top = f64::INFINITY;
    assert!(preview.validate().is_err());
    preview.scroll_top = 0.0;
    preview.content = "x".repeat(MAX_CONTENT_BYTES + 1);
    assert!(preview.validate().is_err());
  }
}
