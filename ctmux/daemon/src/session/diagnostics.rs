//! Record operation boundaries outside the registry/PTY locks. Never log requests.
use super::{PaneMoveError, SessionControlError, SessionManager, SessionManagerError, Terminal};
use ctl_core::observability::{Context, Event, Lease, Level, Operation, Outcome};
use ctmux_proto::{CommandSpec, LeaseKind, LeaseStatus, TerminalSize, ViewInfo, ViewLayout};
use std::sync::Arc;
use uuid::Uuid;

trait Failure {
  fn classification(&self) -> (&'static str, Level);
}

impl Failure for SessionControlError {
  fn classification(&self) -> (&'static str, Level) {
    match self {
      Self::InvalidView(_) => ("view_invalid", Level::Warn),
      Self::InputLeaseRequired => ("input_lease_required", Level::Warn),
      Self::LayoutLeaseRequired => ("layout_lease_required", Level::Warn),
      Self::Io(_) => ("terminal_io_failed", Level::Error),
      Self::Pty(_) => ("pty_failed", Level::Error),
    }
  }
}

impl Failure for SessionManagerError {
  fn classification(&self) -> (&'static str, Level) {
    match self {
      Self::InvalidView(_) => ("view_invalid", Level::Warn),
      Self::InvalidName { .. } => ("session_name_invalid", Level::Warn),
      Self::AlreadyExists { .. } => ("session_exists", Level::Warn),
      Self::NotFound { .. } => ("session_not_found", Level::Warn),
      Self::AutomaticNameExhausted => ("session_names_exhausted", Level::Error),
      Self::Pty(_) => ("pty_failed", Level::Error),
      Self::Spawn(_) => ("pane_spawn_failed", Level::Error),
      #[cfg(unix)]
      Self::ShellReporter(_) => ("shell_reporter_failed", Level::Error),
      Self::ReaderThread(_) => ("pane_reader_failed", Level::Error),
      Self::WaiterThread(_) => ("pane_waiter_failed", Level::Error),
    }
  }
}

impl Failure for PaneMoveError {
  fn classification(&self) -> (&'static str, Level) {
    match self {
      Self::Control(error) => error.classification(),
      Self::Manager(error) => error.classification(),
    }
  }
}

fn logged<T, E: Failure>(
  event: Event,
  level: Level,
  context: Context,
  action: impl FnOnce() -> Result<T, E>,
  describe: impl FnOnce(&T) -> Context,
) -> Result<T, E> {
  let mut operation = Operation::diagnostic_at(event, level, context);
  let result = action();
  match &result {
    Ok(value) => {
      operation.set_context(describe(value));
      operation.finish(Outcome::Succeeded, None, None);
    }
    Err(error) => {
      let (code, level) = error.classification();
      operation.finish_at(Outcome::Failed, level, Some(code), None);
    }
  }
  result
}

fn notice(event: Event, level: Level, context: Context) {
  ctl_core::observability::diagnostic_event(event, level, context, Outcome::Succeeded, None, None);
}

fn view_context(view: &ViewInfo) -> Context {
  Context {
    session_id: Uuid::parse_str(&view.session_id).ok(),
    ..Context::default()
  }
}

impl Terminal {
  fn log_context(&self, attachment_id: Option<&str>) -> Context {
    Context {
      session_id: Uuid::parse_str(&super::lock(&self.owner).session_id).ok(),
      pane_id: Uuid::parse_str(&self.id).ok(),
      attachment_id: attachment_id.and_then(|id| Uuid::parse_str(id).ok()),
      ..Context::default()
    }
  }

  pub fn create_attachment(&self, input: bool, layout: bool) -> super::AttachmentRegistration {
    let mut operation =
      Operation::diagnostic_at(Event::AttachmentCreate, Level::Info, self.log_context(None));
    let result = self.create_attachment_inner(input, layout);
    operation.set_context(self.log_context(Some(&result.attachment_id)));
    operation.finish(Outcome::Succeeded, None, None);
    result
  }

  pub fn resume_attachment(&self, token: &str) -> Option<super::AttachmentRegistration> {
    let mut operation =
      Operation::diagnostic_at(Event::AttachmentResume, Level::Info, self.log_context(None));
    let result = self.resume_attachment_inner(token);
    if let Some(registration) = &result {
      operation.set_context(self.log_context(Some(&registration.attachment_id)));
      operation.finish(Outcome::Succeeded, None, None);
    } else {
      operation.finish_at(
        Outcome::Missing,
        Level::Warn,
        Some("attachment_resume_rejected"),
        None,
      );
    }
    result
  }

  pub fn suspend_attachment(&self, token: &str, generation: u64) -> bool {
    let changed = self.suspend_attachment_inner(token, generation);
    if changed {
      notice(
        Event::AttachmentSuspend,
        Level::Warn,
        self.log_context(None),
      );
    }
    changed
  }

  pub fn expire_attachment(&self, token: &str, generation: u64) {
    if self.expire_attachment_inner(token, generation) {
      notice(Event::AttachmentExpire, Level::Warn, self.log_context(None));
    }
  }

  pub fn close_attachment(&self, token: &str, generation: u64) {
    if self.close_attachment_inner(token, generation) {
      notice(Event::AttachmentDetach, Level::Info, self.log_context(None));
    }
  }

  fn lease_context(&self, attachment_id: &str, lease: LeaseKind) -> Context {
    Context {
      lease: Some(if lease == LeaseKind::Input {
        Lease::Input
      } else {
        Lease::Layout
      }),
      ..self.log_context(Some(attachment_id))
    }
  }

  pub fn acquire_lease(&self, attachment_id: &str, lease: LeaseKind) -> LeaseStatus {
    let result = self.acquire_lease_inner(attachment_id, lease);
    ctl_core::observability::diagnostic_event(
      Event::LeaseAcquire,
      Level::Debug,
      self.lease_context(attachment_id, lease),
      if result.owned_by_client {
        Outcome::Succeeded
      } else {
        Outcome::Failed
      },
      (!result.owned_by_client).then_some("lease_busy"),
      None,
    );
    result
  }

  pub fn release_lease(&self, attachment_id: &str, lease: LeaseKind) -> LeaseStatus {
    let result = self.release_lease_inner(attachment_id, lease);
    notice(
      Event::LeaseRelease,
      Level::Debug,
      self.lease_context(attachment_id, lease),
    );
    result
  }

  pub fn resize(&self, attachment_id: &str, size: TerminalSize) -> Result<(), SessionControlError> {
    let context = self.log_context(Some(attachment_id));
    logged(
      Event::ViewResize,
      Level::Debug,
      context,
      || self.resize_inner(attachment_id, size),
      |()| context,
    )
  }

  pub fn resize_pane(
    &self,
    attachment_id: &str,
    terminal_id: &str,
    direction: ctmux_proto::ResizeDirection,
    amount: u16,
  ) -> Result<ViewInfo, SessionControlError> {
    let context = Context {
      pane_id: Uuid::parse_str(terminal_id).ok(),
      ..self.log_context(Some(attachment_id))
    };
    logged(
      Event::PaneResize,
      Level::Debug,
      context,
      || self.resize_pane_inner(attachment_id, terminal_id, direction, amount),
      |_| context,
    )
  }

  pub fn resize_divider(
    &self,
    attachment_id: &str,
    divider: &ctmux_proto::DividerResize,
  ) -> Result<ViewInfo, SessionControlError> {
    let context = self.log_context(Some(attachment_id));
    logged(
      Event::DividerResize,
      Level::Debug,
      context,
      || self.resize_divider_inner(attachment_id, divider),
      |_| context,
    )
  }

  pub fn set_view_zoom(
    &self,
    attachment_id: &str,
    terminal_id: Option<String>,
  ) -> Result<ViewInfo, SessionControlError> {
    let context = self.log_context(Some(attachment_id));
    logged(
      Event::PaneZoom,
      Level::Info,
      context,
      || self.set_view_zoom_inner(attachment_id, terminal_id),
      |_| context,
    )
  }

  pub fn swap_pane(
    &self,
    attachment_id: &str,
    target: &ctmux_proto::PaneTarget,
    previous: bool,
  ) -> Result<ViewInfo, SessionControlError> {
    let context = self.log_context(Some(attachment_id));
    logged(
      Event::ViewUpdate,
      Level::Info,
      context,
      || self.swap_pane_inner(attachment_id, target, previous),
      |_| context,
    )
  }

  pub fn break_pane(
    &self,
    attachment_id: &str,
    target: &ctmux_proto::PaneTarget,
    name: Option<String>,
  ) -> Result<(ViewInfo, ViewInfo), PaneMoveError> {
    let context = self.log_context(Some(attachment_id));
    logged(
      Event::PanePromote,
      Level::Info,
      context,
      || self.break_pane_inner(attachment_id, target, name),
      |_| context,
    )
  }

  pub fn kill(&self) -> Result<(), SessionControlError> {
    let context = self.log_context(None);
    logged(
      Event::PaneKill,
      Level::Info,
      context,
      || self.kill_inner(),
      |()| context,
    )
  }

  pub(super) fn log_exit(&self, exit_code: Option<u32>) {
    let context = Context {
      exit_code,
      ..self.log_context(None)
    };
    notice(
      Event::PaneExit,
      if exit_code.is_some_and(|code| code != 0) {
        Level::Warn
      } else {
        Level::Info
      },
      context,
    );
  }
}

impl SessionManager {
  pub(super) fn create_terminal(
    &self,
    name: Option<String>,
    command: Option<CommandSpec>,
    cwd: Option<String>,
    size: TerminalSize,
    placement: Option<(String, ctmux_proto::SplitAxis)>,
    managed: bool,
  ) -> Result<Arc<Terminal>, SessionManagerError> {
    if placement.is_some() {
      return self.create_terminal_inner(name, command, cwd, size, placement, managed);
    }
    logged(
      Event::SessionCreate,
      Level::Info,
      Context::default(),
      || self.create_terminal_inner(name, command, cwd, size, placement, managed),
      |terminal| terminal.log_context(None),
    )
  }

  pub fn split_terminal(
    &self,
    terminal_id: String,
    axis: ctmux_proto::SplitAxis,
    command: Option<CommandSpec>,
    working_directory: Option<String>,
    terminal_size: TerminalSize,
  ) -> Result<Arc<Terminal>, SessionManagerError> {
    let context = Context {
      pane_id: Uuid::parse_str(&terminal_id).ok(),
      ..Context::default()
    };
    logged(
      Event::PaneSplit,
      Level::Info,
      context,
      || self.split_terminal_inner(terminal_id, axis, command, working_directory, terminal_size),
      |terminal| terminal.log_context(None),
    )
  }

  pub fn update_view(
    &self,
    selector: &str,
    revision: u64,
    layout: ViewLayout,
  ) -> Result<ViewInfo, SessionManagerError> {
    logged(
      Event::ViewUpdate,
      Level::Info,
      Context::default(),
      || self.update_view_inner(selector, revision, layout),
      view_context,
    )
  }

  pub fn promote_terminal(
    &self,
    id: &str,
    name: Option<String>,
  ) -> Result<ViewInfo, SessionManagerError> {
    logged(
      Event::PanePromote,
      Level::Info,
      Context::default(),
      || self.promote_terminal_inner(id, name),
      view_context,
    )
  }

  pub fn merge_sessions(
    &self,
    source: &str,
    destination: &str,
  ) -> Result<ViewInfo, SessionManagerError> {
    logged(
      Event::SessionMerge,
      Level::Info,
      Context::default(),
      || self.merge_sessions_inner(source, destination),
      view_context,
    )
  }

  pub fn begin_termination(
    &self,
    selector: &str,
  ) -> Result<Vec<Arc<Terminal>>, SessionManagerError> {
    logged(
      Event::SessionTerminate,
      Level::Info,
      Context::default(),
      || self.begin_termination_inner(selector),
      |terminals| {
        terminals
          .first()
          .map_or_else(Context::default, |terminal| terminal.log_context(None))
      },
    )
  }
}
