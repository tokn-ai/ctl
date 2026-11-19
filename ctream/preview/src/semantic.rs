use ctream::{
  protocol::{Action, ActionResult, Element, Target},
  transport::{self, Transport},
};
use eframe::egui;
use std::{
  io,
  sync::{
    Arc,
    mpsc::{Receiver, TryRecvError},
  },
};

enum Pending {
  Elements(Receiver<io::Result<Vec<Element>>>),
  Action(Receiver<io::Result<ActionResult>>),
}

pub struct Panel {
  target: Target,
  window: String,
  elements: Vec<Element>,
  pending: Option<Pending>,
  status: Option<String>,
  text: std::collections::HashMap<String, String>,
}

fn result<T>(receiver: &Receiver<io::Result<T>>) -> Option<io::Result<T>> {
  match receiver.try_recv() {
    Ok(value) => Some(value),
    Err(TryRecvError::Empty) => None,
    Err(TryRecvError::Disconnected) => Some(Err(io::Error::other("agent request ended"))),
  }
}

impl Panel {
  pub fn target(&self) -> Target {
    self.target
  }

  pub fn new(target: Target, window: String) -> Self {
    Self {
      target,
      window,
      elements: Vec::new(),
      pending: None,
      status: None,
      text: Default::default(),
    }
  }

  pub fn show(&mut self, ctx: &egui::Context, transport: &Arc<dyn Transport + Send + Sync>) {
    match &self.pending {
      Some(Pending::Elements(receiver)) => {
        if let Some(response) = result(receiver) {
          self.pending = None;
          self.status = match response {
            Ok(elements) => {
              self.elements = elements;
              self.text.clear();
              self
                .elements
                .is_empty()
                .then(|| "No accessible actions found.".into())
            }
            Err(error) => {
              self.elements.clear();
              Some(format!("Accessible controls unavailable ({error})"))
            }
          };
        }
      }
      Some(Pending::Action(receiver)) => {
        if let Some(response) = result(receiver) {
          self.pending = None;
          self.status = Some(match response {
            Ok(response) if response.applied => "Action applied.".into(),
            Ok(response) => response.error.unwrap_or_else(|| "Action rejected.".into()),
            Err(error) => error.to_string(),
          });
        }
      }
      None => {}
    }
    let mut refresh = false;
    let mut action = None;
    egui::SidePanel::right("window_actions")
      .resizable(true)
      .default_width(230.0)
      .show(ctx, |ui| {
        ui.heading("Window actions");
        ui.add_enabled_ui(self.pending.is_none(), |ui| {
          refresh = ui.button("Inspect controls").clicked();
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
                    action = Some(Action::Invoke {
                      path: element.path.clone(),
                      index,
                      name: name.clone(),
                    });
                  }
                }
                if element.editable {
                  let text = self.text.entry(element.path.clone()).or_default();
                  ui.text_edit_singleline(text);
                  if ui.button("Set text").clicked() {
                    action = Some(Action::SetText {
                      path: element.path.clone(),
                      text: text.clone(),
                    });
                  }
                }
                ui.separator();
              });
            }
          });
        });
        if self.pending.is_some() {
          ui.spinner();
        }
        if let Some(status) = &self.status {
          ui.label(status);
        }
      });
    let target = self.target;
    if refresh {
      let transport = Arc::clone(transport);
      let id = self.window.clone();
      self.pending = Some(Pending::Elements(super::background(ctx, move || {
        transport::elements(transport.as_ref(), target, id)
      })));
    } else if let Some(action) = action {
      let transport = Arc::clone(transport);
      let id = self.window.clone();
      self.pending = Some(Pending::Action(super::background(ctx, move || {
        transport::action(transport.as_ref(), target, id, action)
      })));
    }
  }
}
