//! Complete input requests executed by the desktop daemon against a selected source.
use crate::native_input;
use ctream_client::{
  Client,
  automation::Automation,
  input::{Input, InputFormat},
  sdk::Mutation,
  transport::{Channel, Local, Transport},
};
use ctream_proto::{Target, Window, live::LiveAction};
use serde::{Deserialize, Serialize};
use std::{
  collections::BTreeSet,
  io::{self, Read, Write},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputRequest {
  Sdk { action: Mutation },
  Raw { commands: Vec<String> },
}

impl InputRequest {
  pub fn operation(&self) -> &'static str {
    match self {
      Self::Sdk { action } => action.overlay().operation(),
      Self::Raw { commands } => native_input::operation(commands),
    }
  }

  pub fn validate(&self) -> io::Result<()> {
    match self {
      Self::Sdk { action } if !action.overlay().valid() => Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "pointer is outside the screenshot",
      )),
      Self::Sdk { .. } => Ok(()),
      Self::Raw { commands } => {
        if !(1..=native_input::BATCH_LIMIT).contains(&commands.len()) {
          return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid native input batch length",
          ));
        }
        for command in commands {
          native_input::validate_event(command)?;
        }
        Ok(())
      }
    }
  }

  pub fn overlay(&self) -> Option<LiveAction> {
    match self {
      Self::Sdk { action } => Some(action.overlay()),
      Self::Raw { commands } => RawState::default().overlay(commands),
    }
  }
}

/// Mac window control is bound to the capture that authorized the exact window and PID.
/// The observer and the daemon worker each hold a private subscription; only the worker
/// receives native input delegation.
pub struct DesktopTransport {
  local: Local,
  delegation: Option<String>,
  #[cfg(target_os = "macos")]
  lease: String,
}

impl DesktopTransport {
  pub fn new(delegation: Option<String>) -> io::Result<Self> {
    Ok(Self {
      local: Local {
        agent: std::env::current_exe()?,
      },
      delegation,
      #[cfg(target_os = "macos")]
      lease: uuid::Uuid::new_v4().simple().to_string(),
    })
  }
}

impl Transport for DesktopTransport {
  type Reader = <Local as Transport>::Reader;
  type Writer = <Local as Transport>::Writer;

  fn open(&self, args: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    #[cfg(target_os = "macos")]
    let owned;
    #[cfg(target_os = "macos")]
    let args = match args {
      [version, operation, target, window] if target == "macos" => {
        let scoped = match (version.as_str(), operation.as_str()) {
          (ctream_proto::MACOS_WINDOW_VERSION, "capture-window") => Some("session-capture"),
          (ctream_proto::INPUT_SYNC_VERSION, "input-sync") => Some("session-input"),
          (_, "input-window") => {
            return Err(io::Error::other(
              "named input requires an updated macOS desktop agent",
            ));
          }
          _ => None,
        };
        if let Some(operation) = scoped {
          owned = [
            ctream_proto::ATTRIBUTED_SESSION_VERSION.into(),
            operation.into(),
            target.clone(),
            window.clone(),
            self.lease.clone(),
          ];
          &owned[..]
        } else {
          args
        }
      }
      _ => args,
    };
    match &self.delegation {
      Some(key) => self.local.open_delegated(args, key),
      None => self.local.open(args),
    }
  }
}

type RawInput = Input<Box<dyn Read + Send>, Box<dyn Write + Send>>;

#[derive(Default)]
struct RawState {
  dimensions: Option<[u32; 2]>,
  point: Option<[f32; 2]>,
  held: BTreeSet<(bool, String)>,
}

impl RawState {
  fn overlay(&self, commands: &[String]) -> Option<LiveAction> {
    let mut point = self.point;
    let mut event = None;
    for line in commands {
      let mut words = line.split_ascii_whitespace();
      match words.next()? {
        "move" => {
          let (x, y) = (
            words.next()?.parse::<u32>().ok()?,
            words.next()?.parse::<u32>().ok()?,
          );
          let size = match words.next() {
            Some(w) => [w.parse().ok()?, words.next()?.parse().ok()?],
            None => self.dimensions?,
          };
          if x >= size[0] || y >= size[1] {
            return None;
          }
          let p = [
            (x as f32 + 0.5) / size[0] as f32,
            (y as f32 + 0.5) / size[1] as f32,
          ];
          point = Some(p);
          event = Some(LiveAction::Move { point: p });
        }
        "button" => {
          let (code, edge) = (words.next()?, words.next()?);
          if let Some(point) = point {
            event = Some(match (code, edge) {
              ("4" | "5" | "6" | "7", _) => LiveAction::Scroll { point },
              (_, "1") => LiveAction::PointerDown { point },
              _ => LiveAction::PointerUp { point },
            });
          }
        }
        "key" => {
          event.get_or_insert(LiveAction::PressKeys {});
        }
        _ => {}
      }
    }
    event
  }

  fn delivered(&mut self, commands: &[String]) {
    native_input::update_held(&mut self.held, commands);
    if let Some(last_move) = commands
      .iter()
      .rev()
      .find(|line| line.split_ascii_whitespace().next() == Some("move"))
      && let Some(LiveAction::Move { point }) = self.overlay(std::slice::from_ref(last_move))
    {
      self.point = Some(point);
    }
  }
}

#[derive(Default, PartialEq)]
struct RawRoute {
  picker: Option<String>,
  lease: Option<String>,
}

pub struct DesktopDriver {
  target: Target,
  window: Option<String>,
  delegation: String,
  automation: Option<Automation<DesktopTransport>>,
  raw: Option<RawInput>,
  route: RawRoute,
  state: RawState,
}

impl DesktopDriver {
  pub fn new(target: Target, window: Option<String>, delegation_key: String) -> Self {
    Self {
      target,
      window,
      delegation: delegation_key,
      automation: None,
      raw: None,
      route: RawRoute::default(),
      state: RawState::default(),
    }
  }

  pub fn raw_held(&self) -> bool {
    !self.state.held.is_empty()
  }

  pub fn overlay(&self, request: &InputRequest) -> Option<LiveAction> {
    match request {
      InputRequest::Sdk { action } => Some(action.overlay()),
      InputRequest::Raw { commands } => self.state.overlay(commands),
    }
  }

  pub fn raw_header(&mut self, picker: Option<&str>, lease: Option<&str>) -> io::Result<String> {
    if picker.is_some() && lease.is_some()
      || self.target != Target::MacosDesktop && (picker.is_some() || lease.is_some())
    {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid input worker selection",
      ));
    }
    let route = RawRoute {
      picker: picker.map(str::to_owned),
      lease: lease.map(str::to_owned),
    };
    if self.raw.is_some() && self.route != route {
      return Err(io::Error::other("input source is already open"));
    }
    self.route = route;
    self.open_raw()?;
    Ok(match self.state.dimensions {
      Some([w, h]) => format!("{w} {h}\n"),
      None => "ready\n".into(),
    })
  }

  fn open_raw(&mut self) -> io::Result<&mut RawInput> {
    if self.raw.is_none() {
      let format = match (self.target, self.window.as_deref()) {
        (Target::X11(_), None) => InputFormat::X11,
        (Target::X11(_), Some(_)) => InputFormat::X11Window,
        (Target::Wayland(_), None) => InputFormat::Wayland,
        (Target::MacosDesktop, Some(_)) => InputFormat::Macos,
        (Target::Windows(_), _) | (Target::WindowsDesktop, Some(_)) => InputFormat::Windows,
        _ => {
          return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "source does not support native input",
          ));
        }
      };
      let channel = self.raw_channel()?;
      let input = Input::open(channel, format)?;
      self.state.dimensions = input.dimensions();
      self.raw = Some(input);
    }
    Ok(self.raw.as_mut().expect("opened native channel"))
  }

  fn raw_channel(&self) -> io::Result<Channel<Box<dyn Read + Send>, Box<dyn Write + Send>>> {
    #[cfg(target_os = "macos")]
    if let Some(id) = &self.route.picker {
      let stream = crate::native_input::picker::open(
        id,
        self
          .window
          .as_deref()
          .ok_or_else(|| io::Error::other("picker input requires a window"))?,
        &self.delegation,
      )?;
      let writer = PickerWriter(stream.try_clone()?);
      let cancel = stream.try_clone()?;
      return Ok(Channel {
        reader: Box::new(stream),
        writer: Box::new(writer),
        cancellation: ctream_client::transport::Cancellation::new(move || {
          let _ = cancel.shutdown(std::net::Shutdown::Both);
        }),
      });
    }
    #[cfg(not(target_os = "macos"))]
    if self.route.picker.is_some() {
      return Err(io::Error::other("picker input requires macOS"));
    }
    let mut args = vec![
      ctream_proto::INPUT_SYNC_VERSION.into(),
      "input-sync".into(),
      self.target.to_string(),
    ];
    if let Some(window) = &self.window {
      args.push(window.clone());
    }
    if let Some(lease) = &self.route.lease {
      args[0] = ctream_proto::ATTRIBUTED_SESSION_VERSION.into();
      args[1] = "session-input".into();
      args.push(lease.clone());
    }
    let channel = Local {
      agent: std::env::current_exe()?,
    }
    .open_delegated(&args, &self.delegation)?;
    Ok(Channel {
      reader: Box::new(channel.reader),
      writer: Box::new(channel.writer),
      cancellation: channel.cancellation,
    })
  }

  pub fn reset_raw(&mut self) -> io::Result<()> {
    self.state.held.clear();
    if let Some(input) = self.raw.take() {
      input.finish_bounded()?;
    }
    Ok(())
  }

  pub fn execute(&mut self, request: InputRequest) -> io::Result<()> {
    request.validate()?;
    match request {
      InputRequest::Sdk { action } => {
        if self.automation.is_none() {
          let client = Client::new(DesktopTransport::new(Some(self.delegation.clone()))?);
          let window = self.window.clone().map(|id| Window {
            id,
            ..Default::default()
          });
          self.automation = Some(client.session(self.target, window).automation()?);
        }
        action.apply(self.automation.as_mut().expect("opened SDK worker"))
      }
      InputRequest::Raw { commands } => {
        let delivered = self.open_raw()?.send_native_batch(&commands);
        if let Err(error) = delivered {
          let _ = self.reset_raw();
          return Err(error);
        }
        self.state.delivered(&commands);
        Ok(())
      }
    }
  }
}

#[cfg(target_os = "macos")]
struct PickerWriter(std::os::unix::net::UnixStream);

#[cfg(target_os = "macos")]
impl Write for PickerWriter {
  fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
    self.0.write(bytes)
  }
  fn flush(&mut self) -> io::Result<()> {
    self.0.flush()
  }
}

#[cfg(target_os = "macos")]
impl Drop for PickerWriter {
  fn drop(&mut self) {
    let _ = self.0.shutdown(std::net::Shutdown::Write);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn raw_overlay_uses_confirmed_source_coordinates_across_batches() {
    let mut state = RawState {
      dimensions: Some([10, 10]),
      ..Default::default()
    };
    let movement = vec!["move 1 2\n".to_owned()];
    let point = [0.15, 0.25];
    assert_eq!(state.overlay(&movement), Some(LiveAction::Move { point }));
    let down = vec!["button 1 1\n".to_owned()];
    assert_eq!(state.overlay(&down), None);
    state.delivered(&movement);
    assert_eq!(
      state.overlay(&down),
      Some(LiveAction::PointerDown { point })
    );
    state.delivered(&down);
    assert_eq!(state.held.len(), 1);
    state.delivered(&["button 1 0\n".into()]);
    assert!(state.held.is_empty());
  }

  #[test]
  fn raw_batches_reject_embedded_commands_before_any_worker_is_opened() {
    for lines in [
      vec![],
      vec!["".into()],
      vec!["key a 1\nreset\n".into()],
      vec!["key a 1".into()],
      vec!["key x 1\n".into(); native_input::BATCH_LIMIT + 1],
      vec!["sync\n".into()],
    ] {
      assert!(InputRequest::Raw { commands: lines }.validate().is_err());
    }
  }
}
