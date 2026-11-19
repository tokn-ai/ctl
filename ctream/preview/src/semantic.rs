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
  inspected_size: Option<[usize; 2]>,
}

fn result<T>(receiver: &Receiver<io::Result<T>>) -> Option<io::Result<T>> {
  match receiver.try_recv() {
    Ok(value) => Some(value),
    Err(TryRecvError::Empty) => None,
    Err(TryRecvError::Disconnected) => Some(Err(io::Error::other("agent request ended"))),
  }
}

impl Panel {
  pub fn new(target: Target, window: String) -> Self {
    Self {
      target,
      window,
      elements: Vec::new(),
      pending: None,
      status: None,
      text: Default::default(),
      inspected_size: None,
    }
  }

  pub fn overlay_elements(&self) -> &[Element] {
    if self.inspected_size.is_some() && !matches!(self.pending, Some(Pending::Elements(_))) {
      &self.elements
    } else {
      &[]
    }
  }

  pub fn show(
    &mut self,
    ctx: &egui::Context,
    transport: &Arc<dyn Transport + Send + Sync>,
    frame_size: Option<[usize; 2]>,
  ) {
    // The control geometry is a snapshot of the remote layout. Local panel and
    // window resizes keep the same frame size and can reuse the snapshot.
    if self.inspected_size != frame_size {
      self.inspected_size = None;
    }
    match &self.pending {
      Some(Pending::Elements(receiver)) => {
        if let Some(response) = result(receiver) {
          self.pending = None;
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
      self.inspected_size = frame_size;
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
