use super::live::{Publisher, viewer_id};
use ctream_client::{
  Session,
  semantic_session::{SemanticSession, SemanticTurn},
  transport::Transport,
};
use ctream_proto::{
  Action, Element,
  live::{DEFAULT_TURN_MS, LiveAction},
};
use eframe::egui;
use std::{
  io,
  sync::{
    Arc, Mutex,
    mpsc::{Receiver, TryRecvError},
  },
  time::{Duration, Instant},
};

pub const PANEL_WIDTH: f32 = 230.0;

enum Command {
  Connect,
  Acquire,
  Release,
  Apply(Action),
}

struct Feedback {
  turn: SemanticTurn,
  status: Option<String>,
}

pub struct Panel<T: Transport> {
  session: Session<T>,
  activity: Publisher,
  control: Arc<Mutex<SemanticSession<T>>>,
  turn: SemanticTurn,
  pending: Option<Receiver<io::Result<Feedback>>>,
  inspection: Option<Receiver<io::Result<Vec<Element>>>>,
  elements: Vec<Element>,
  status: Option<String>,
  text: std::collections::HashMap<String, String>,
  inspected_size: Option<[usize; 2]>,
}

fn result<T>(pending: &mut Option<Receiver<io::Result<T>>>) -> Option<io::Result<T>> {
  let value = match pending.as_ref()?.try_recv() {
    Ok(value) => value,
    Err(TryRecvError::Empty) => return None,
    Err(TryRecvError::Disconnected) => Err(io::Error::other("agent request ended")),
  };
  *pending = None;
  Some(value)
}

impl<T: Transport + Send + Sync + 'static> Panel<T> {
  pub fn new(session: Session<T>, activity: Publisher) -> Self {
    Self {
      control: Arc::new(Mutex::new(SemanticSession::new(session.clone()))),
      session,
      activity,
      turn: SemanticTurn::Connecting,
      pending: None,
      inspection: None,
      elements: Vec::new(),
      status: None,
      text: Default::default(),
      inspected_size: None,
    }
  }

  pub fn overlay_elements(&self) -> &[Element] {
    if self.inspected_size.is_some() && self.inspection.is_none() {
      &self.elements
    } else {
      &[]
    }
  }

  fn command(&mut self, ctx: &egui::Context, command: Command) {
    let control = self.control.clone();
    let activity = self.activity.clone();
    // This mutex is only acquired on workers. The view's Requests guard owns
    // cancellation even if the panel is closed during a pending host request.
    self.pending = Some(super::background_for(ctx, ctx.viewport_id(), move || {
      let mut control = control
        .lock()
        .map_err(|_| io::Error::other("semantic controller stopped"))?;
      let reply = match command {
        Command::Connect => control
          .connect_viewer(viewer_id(), activity.attachment()?.as_ref())
          .map(|()| None),
        Command::Acquire => control.acquire(DEFAULT_TURN_MS).map(|()| None),
        Command::Release => control.release().map(|()| None),
        Command::Apply(action) => {
          let event = match &action {
            Action::Invoke { .. } => LiveAction::Invoke {},
            Action::SetText { .. } => LiveAction::SetText {},
          };
          control.apply(action).map(|reply| {
            if reply.applied {
              // Current hosts publish the attributed action and its audit ID.
              if control.state() == SemanticTurn::Legacy {
                activity.send(event);
              }
              Some("Action applied.".into())
            } else {
              Some(reply.error.unwrap_or_else(|| "Action rejected.".into()))
            }
          })
        }
      };
      Ok(Feedback {
        turn: control.state(),
        status: reply.unwrap_or_else(|error| Some(error.to_string())),
      })
    }));
  }

  pub fn show(&mut self, ui: &mut egui::Ui, frame_size: Option<[usize; 2]>) {
    if self.inspected_size != frame_size {
      self.inspected_size = None;
    }
    if let Some(response) = result(&mut self.inspection) {
      self.status = match response {
        Ok(elements) => {
          self.elements = elements;
          self.text.clear();
          if self.elements.is_empty() {
            Some("No accessible actions found.".into())
          } else if self.elements.iter().all(|element| element.bounds.is_none()) {
            Some("No control locations available.".into())
          } else {
            None
          }
        }
        Err(error) => {
          self.elements.clear();
          self.inspected_size = None;
          Some(format!("Accessible controls unavailable ({error})"))
        }
      };
    }
    if let Some(response) = result(&mut self.pending) {
      match response {
        Ok(feedback) => {
          self.turn = feedback.turn;
          self.status = feedback.status;
        }
        Err(error) => {
          self.turn = SemanticTurn::Disconnected;
          self.status = Some(error.to_string());
        }
      }
    }
    if self.turn == SemanticTurn::Connecting && self.pending.is_none() {
      self.command(ui.ctx(), Command::Connect);
    }
    if let SemanticTurn::Held(end) = self.turn
      && end <= Instant::now()
    {
      self.turn = SemanticTurn::Expired;
    }

    let mut refresh = false;
    let mut command = None;
    egui::Panel::right("window_actions")
      .resizable(true)
      .default_size(PANEL_WIDTH)
      .show(ui, |ui| {
        ui.heading("Window actions");
        ui.add_enabled_ui(self.pending.is_none(), |ui| match self.turn {
          SemanticTurn::Available | SemanticTurn::Expired => {
            if ui
              .button("Take turn")
              .on_hover_text("Reserve desktop input for 10 seconds")
              .clicked()
            {
              command = Some(Command::Acquire);
            }
            if self.turn == SemanticTurn::Expired {
              ui.label("The turn expired. Take a new turn or release it.");
              if ui.button("Release turn").clicked() {
                command = Some(Command::Release);
              }
            }
          }
          SemanticTurn::Held(end) => {
            let remaining = end.saturating_duration_since(Instant::now());
            ui.label(format!(
              "Turn expires in {} s",
              remaining.as_millis().div_ceil(1000)
            ));
            ui.ctx()
              .request_repaint_after(remaining.min(Duration::from_secs(1)));
            if ui.button("Release turn").clicked() {
              command = Some(Command::Release);
            }
          }
          SemanticTurn::Legacy => {
            ui.weak("Control turns require ctream-agent 1.1.18.");
          }
          SemanticTurn::Connecting | SemanticTurn::Disconnected => {}
        });
        refresh = ui
          .add_enabled(
            self.inspection.is_none(),
            egui::Button::new("Inspect controls"),
          )
          .clicked();
        let can_apply = matches!(
          self.turn,
          SemanticTurn::Legacy | SemanticTurn::Available | SemanticTurn::Held(_)
        );
        ui.add_enabled_ui(
          can_apply && self.pending.is_none() && self.inspection.is_none(),
          |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
              for element in &self.elements {
                ui.push_id(&element.path, |ui| {
                  ui.label(if element.name.is_empty() {
                    &element.role
                  } else {
                    &element.name
                  });
                  for (index, name) in element.actions.iter().enumerate() {
                    if ui.button(name).clicked() {
                      command = Some(Command::Apply(Action::Invoke {
                        path: element.path.clone(),
                        index,
                        name: name.clone(),
                      }));
                    }
                  }
                  if element.editable {
                    let text = self.text.entry(element.path.clone()).or_default();
                    ui.text_edit_singleline(text);
                    if ui.button("Set text").clicked() {
                      command = Some(Command::Apply(Action::SetText {
                        path: element.path.clone(),
                        text: text.clone(),
                      }));
                    }
                  }
                  ui.separator();
                });
              }
            });
          },
        );
        if self.pending.is_some() || self.inspection.is_some() {
          ui.spinner();
        }
        if let Some(status) = &self.status {
          ui.label(status);
        }
      });
    let ctx = ui.ctx();
    if refresh {
      self.inspected_size = frame_size;
      let session = self.session.clone();
      self.inspection = Some(super::background_for(ctx, ctx.viewport_id(), move || {
        session.elements()
      }));
    }
    if let Some(command) = command {
      self.command(ctx, command);
    }
  }
}
