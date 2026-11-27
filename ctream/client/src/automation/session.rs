use super::{ActiveCapture, Controls, closed};
use crate::{Session, media::Frame, transport::Transport};
use ctream_proto::{Action, ActionResult, Target};
use std::{io, sync::Arc};

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
  Unprobed,
  Synchronized,
  Legacy,
  Failed,
}

/// A selected source with independently lazy capture and input channels.
pub struct Automation<T: Transport> {
  session: Session<T>,
  controls: Option<Controls<T::Reader, T::Writer>>,
  capture: Option<ActiveCapture<T::Writer>>,
  mode: InputMode,
}

impl<T: Transport> Session<T> {
  /// Select a source for repeated observations and actions. The first request
  /// for each capability opens its channel.
  pub fn automation(&self) -> io::Result<Automation<T>> {
    self.validate_target()?;
    Ok(Automation {
      session: self.clone(),
      controls: None,
      capture: None,
      mode: InputMode::Unprobed,
    })
  }
}

impl<T: Transport> Automation<T> {
  pub fn selection(&self) -> &Session<T> {
    &self.session
  }

  fn capture(&mut self) -> io::Result<&mut ActiveCapture<T::Writer>> {
    if self.capture.is_none() {
      self.capture = Some(ActiveCapture::start(self.session.capture()?)?);
    }
    Ok(self.capture.as_mut().expect("opened capture"))
  }

  /// Observe the latest complete RGB24 frame. The initial frame may take up
  /// to 30 seconds. Later observations prefer a frame newer than the previous
  /// screenshot. After an action, prefer a frame received after the screenshot
  /// request begins. An idle source returns its last frame after 250 ms.
  /// Capture termination is an error.
  pub fn screenshot(&mut self) -> io::Result<Arc<Frame>> {
    self.capture()?.screenshot()
  }

  fn after_action(&mut self) {
    if let Some(capture) = self.capture.as_mut() {
      capture.after_action();
    }
  }

  /// Preserve observation freshness when a host-owned worker dispatches the input.
  pub(crate) fn prepare_remote_action(&mut self) -> io::Result<()> {
    if let Some(capture) = &self.capture {
      capture.check()?;
    }
    self.after_action();
    Ok(())
  }

  /// Apply an accessibility action to the selected window. The next
  /// screenshot waits for an update using the same policy as physical input.
  pub fn action(&mut self, action: Action) -> io::Result<ActionResult> {
    if let Some(capture) = &self.capture {
      capture.check()?;
    }
    self.after_action();
    self.session.action(action)
  }

  /// Run an input batch, then wait for the agent to acknowledge it. Older
  /// agents acknowledge completion by closing a per-batch input channel.
  /// Errors never replay a batch. A failed stream requires a new automation.
  pub fn with_controls(
    &mut self,
    action: impl FnOnce(&mut Controls<T::Reader, T::Writer>) -> io::Result<()>,
  ) -> io::Result<()> {
    if !self.session.supports_input() {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "selected source does not support physical input",
      ));
    }
    if self.mode == InputMode::Failed {
      return Err(closed());
    }
    if self.session.target == Target::MacosDesktop {
      // Input must bind to this exact window's active ScreenCaptureKit lease.
      self.capture()?.ready()?;
    } else if let Some(capture) = &self.capture {
      capture.check()?;
    }
    if self.mode == InputMode::Unprobed {
      self.mode = if self.session.supports_input_sync()? {
        InputMode::Synchronized
      } else {
        InputMode::Legacy
      };
    }
    let mut controls = match self.controls.take() {
      Some(controls) => controls,
      None => {
        let input = if self.mode == InputMode::Synchronized {
          self.session.synchronized_input()?
        } else {
          self.session.input()?
        };
        Controls {
          input: Some(input),
          capture: None,
        }
      }
    };

    self.after_action();
    let action_result = action(&mut controls);
    if controls.input.is_none() {
      self.mode = InputMode::Failed;
      return Err(action_result.err().unwrap_or_else(closed));
    }
    // A user closure can perform a valid write before returning its own error.
    // Fence those writes even when client-side validation rejected a later one.
    let completed = if self.mode == InputMode::Synchronized {
      let result = controls.send(|input| input.synchronize());
      if result.is_ok() {
        self.controls = Some(controls);
      }
      result
    } else {
      controls.finish()
    };
    if let Err(error) = completed {
      self.mode = InputMode::Failed;
      return Err(error);
    }
    action_result
  }

  /// Release input before capture and wait for the agent to finish both.
  pub fn finish(mut self) -> io::Result<()> {
    self.close()
  }

  // Only synchronized channels live between actions. Their last batch was
  // already acknowledged, so teardown can bound its wait for the peer.
  fn close_idle_controls(mut controls: Controls<T::Reader, T::Writer>) -> io::Result<()> {
    let input = controls.input.take().ok_or_else(closed)?;
    input.finish_bounded()
  }

  fn close(&mut self) -> io::Result<()> {
    let input = self
      .controls
      .take()
      .map(Self::close_idle_controls)
      .transpose();
    let capture = self
      .capture
      .take()
      .map(|mut capture| capture.finish())
      .transpose();
    input.and(capture).map(|_| ())
  }
}

impl<T: Transport> Drop for Automation<T> {
  fn drop(&mut self) {
    let _ = self.close();
  }
}
