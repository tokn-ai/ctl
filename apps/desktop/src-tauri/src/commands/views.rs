use super::client_identity;
use crate::dto::{ConnectionTargetDto, TerminalSizeDto};
use crate::error::{CommandErrorDto, CommandResult};
use crate::transport;
use ctmux_client::session::SessionId;
use ctmux_client::view::{SplitTerminalRequest, TerminalId, UpdateViewRequest, ViewClient};
use ctmux_proto::{SplitAxis, ViewLayout};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct ViewRequest {
  target: ConnectionTargetDto,
  action: ViewAction,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewAction {
  Get {
    session_id: String,
  },
  Split {
    terminal_id: String,
    axis: SplitAxis,
    terminal_size: TerminalSizeDto,
    working_directory: Option<String>,
  },
  Update {
    session_id: String,
    expected_revision: String,
    layout: ViewLayout,
  },
  Promote {
    terminal_id: String,
    name: Option<String>,
  },
  Merge {
    source: String,
    destination: String,
  },
  KillTerminal {
    terminal_id: String,
  },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViewDto {
  session_name: String,
  view_id: String,
  session_id: String,
  revision: String,
  canvas_size: TerminalSizeDto,
  zoomed_terminal_id: Option<String>,
  panes: Vec<ctmux_proto::PaneGeometry>,
  layout: ViewLayout,
  terminals: Vec<TerminalDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalDto {
  terminal_id: String,
  name: String,
  next_sequence: String,
  terminal_size: TerminalSizeDto,
}

impl From<ctmux_proto::ViewInfo> for ViewDto {
  fn from(view: ctmux_proto::ViewInfo) -> Self {
    Self {
      session_name: view.session_name,
      view_id: view.view_id,
      session_id: view.session_id,
      revision: view.revision.to_string(),
      canvas_size: view.canvas_size.into(),
      zoomed_terminal_id: view.zoomed_terminal_id,
      panes: view.panes,
      layout: view.layout,
      terminals: view
        .terminals
        .into_iter()
        .map(|terminal| TerminalDto {
          terminal_id: terminal.terminal_id,
          name: terminal.name,
          next_sequence: terminal.next_sequence.to_string(),
          terminal_size: terminal.terminal_size.into(),
        })
        .collect(),
    }
  }
}

#[tauri::command]
pub async fn session_view(request: ViewRequest) -> CommandResult<Option<ViewDto>> {
  let result = match request.action {
    ViewAction::Get { session_id } => view_client(&request.target)
      .await?
      .get(SessionId(session_id))
      .await
      .map(Some),
    ViewAction::Split {
      terminal_id,
      axis,
      terminal_size,
      working_directory,
    } => {
      // Validate DTO values before connecting or requesting authentication.
      let parameters = SplitTerminalRequest {
        terminal_id: TerminalId(terminal_id),
        axis,
        command: Vec::new(),
        cwd: working_directory,
        terminal_size: terminal_size.into_proto()?,
      };
      view_client(&request.target)
        .await?
        .split(parameters)
        .await
        .map(Some)
    }
    ViewAction::Update {
      session_id,
      expected_revision,
      layout,
    } => {
      let parameters = UpdateViewRequest {
        session_id: SessionId(session_id),
        expected_revision: expected_revision
          .parse()
          .map_err(CommandErrorDto::backend)?,
        layout,
      };
      view_client(&request.target)
        .await?
        .update(parameters)
        .await
        .map(Some)
    }
    ViewAction::Promote { terminal_id, name } => view_client(&request.target)
      .await?
      .promote(TerminalId(terminal_id), name)
      .await
      .map(Some),
    ViewAction::Merge {
      source,
      destination,
    } => view_client(&request.target)
      .await?
      .merge(SessionId(source), SessionId(destination))
      .await
      .map(Some),
    ViewAction::KillTerminal { terminal_id } => view_client(&request.target)
      .await?
      .terminate_terminal(TerminalId(terminal_id))
      .await
      .map(|()| None),
  };
  result
    .map(|view| view.map(ViewDto::from))
    .map_err(view_error)
}

async fn view_client(
  target: &ConnectionTargetDto,
) -> CommandResult<ViewClient<ctl_client::Transport>> {
  Ok(ViewClient::new(
    transport::connect(target).await?,
    client_identity(),
  ))
}

fn view_error(error: ctmux_client::ClientError) -> CommandErrorDto {
  match error {
    ctmux_client::ClientError::UnexpectedResponse { expected, .. } => CommandErrorDto::new(
      "unexpected_ctmux_response",
      format!("expected {expected}, received another response type"),
    ),
    error => CommandErrorDto::client(error),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctmux_proto::{PaneGeometry, TerminalInfo, TerminalSize, ViewInfo};

  #[test]
  fn view_errors_preserve_desktop_codes_without_exposing_response_contents() {
    for expected in ["view_snapshot", "success"] {
      let error = view_error(ctmux_client::ClientError::UnexpectedResponse {
        expected,
        actual: "private response contents".into(),
      });
      assert_eq!(error.code, "unexpected_ctmux_response");
      assert_eq!(
        error.message,
        format!("expected {expected}, received another response type")
      );
    }
    let error = view_error(ctmux_client::ClientError::Server {
      code: ctmux_proto::ErrorCode::InvalidRequest,
      message: "view revision changed".into(),
    });
    assert_eq!(error.code, "invalid_request");
    assert_eq!(error.message, "view revision changed");
  }

  #[test]
  fn view_event_keeps_hidden_members_and_base_geometry_while_reporting_zoom() {
    let canvas = TerminalSize {
      columns: 80,
      rows: 24,
      pixel_width: 0,
      pixel_height: 0,
    };
    let view = ViewInfo {
      session_name: "shell".into(),
      view_id: "view".into(),
      session_id: "session".into(),
      revision: u64::MAX,
      canvas_size: canvas.clone(),
      zoomed_terminal_id: Some("secondary".into()),
      panes: vec![
        PaneGeometry {
          terminal_id: "primary".into(),
          left: 0,
          top: 0,
          columns: 40,
          rows: 24,
        },
        PaneGeometry {
          terminal_id: "secondary".into(),
          left: 41,
          top: 0,
          columns: 39,
          rows: 24,
        },
      ],
      layout: ViewLayout::Split {
        axis: SplitAxis::Horizontal,
        weights: vec![2, 1],
        children: vec![
          ViewLayout::Terminal {
            terminal_id: "primary".into(),
          },
          ViewLayout::Terminal {
            terminal_id: "secondary".into(),
          },
        ],
      },
      terminals: ["primary", "secondary"]
        .into_iter()
        .map(|id| TerminalInfo {
          terminal_id: id.into(),
          name: id.into(),
          created_at_ms: 0,
          next_sequence: u64::MAX,
          terminal_size: canvas.clone(),
        })
        .collect(),
    };
    let event = crate::dto::AttachmentEventDto::ViewChanged {
      attachment_id: "owner".into(),
      view: view.clone().into(),
    };
    let serialized = serde_json::to_value(event).unwrap();
    assert_eq!(serialized["event_type"], "view_changed");
    assert_eq!(serialized["view"]["zoomed_terminal_id"], "secondary");
    assert_eq!(serialized["view"]["revision"], u64::MAX.to_string());
    assert_eq!(serialized["view"]["panes"][1]["columns"], 39);
    assert_eq!(
      serialized["view"]["layout"]["weights"],
      serde_json::json!([2, 1])
    );
    assert_eq!(serialized["view"]["terminals"].as_array().unwrap().len(), 2);
    let cleared = ViewDto::from(ViewInfo {
      zoomed_terminal_id: None,
      ..view
    });
    assert!(serde_json::to_value(cleared).unwrap()["zoomed_terminal_id"].is_null());
  }
}
