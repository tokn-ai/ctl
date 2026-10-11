use super::{App, NoticeKind, Overlay};
use crate::{
  Result,
  prompt::{Event as PromptEvent, Prompt},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ctmux_client::session::SessionId;
use ctmux_proto::SessionInfo;
use std::{
  fmt::Write as _,
  time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::{Instant, timeout};

/// Identity and display label captured before editing or confirming a mutation.
/// Name changes or reordered lists must never redirect the requested action.
#[derive(Clone)]
pub(super) struct Target {
  pub id: String,
  pub name: String,
}

impl From<&SessionInfo> for Target {
  fn from(session: &SessionInfo) -> Self {
    Self {
      id: session.session_id.clone(),
      name: session.name.clone(),
    }
  }
}

pub(super) struct NameEntry {
  target: Option<Target>,
  selected: Option<String>,
  pub input: Prompt,
}

impl App<'_> {
  pub(crate) async fn start_resume(&mut self) -> Result<()> {
    self.list().await?;
    if self.sessions.is_empty() && !self.read_only {
      self.create().await?;
    } else {
      self.open_sessions();
    }
    Ok(())
  }

  pub(super) async fn resolve_session_target(&mut self, selector: Option<&str>) -> Result<Target> {
    self.list().await?;
    let selector = selector
      .or_else(|| self.view.as_ref().map(|view| view.session_id.as_str()))
      .ok_or("No active session; specify -t NAME_OR_ID")?;
    self
      .sessions
      .iter()
      .find(|session| session.session_id == selector)
      .or_else(|| {
        self
          .sessions
          .iter()
          .find(|session| session.name == selector)
      })
      .map(Target::from)
      .ok_or_else(|| format!("Session not found: {selector}").into())
  }

  pub(super) async fn rename_session(&mut self, target: &Target, name: String) -> Result<()> {
    if self.read_only {
      return Err("This attachment is read only".into());
    }
    // SessionInfo has no revision. Drop an older pending list observation
    // before updating its name; view snapshots have their own ordered revision.
    self.maintenance.cancel_refresh();
    let session = timeout(Duration::from_secs(5), async {
      Ok::<_, crate::Error>(
        self
          .session_client()
          .await?
          .rename(SessionId(target.id.clone()), name)
          .await?,
      )
    })
    .await??;
    if let Some(current) = self
      .sessions
      .iter_mut()
      .find(|current| current.session_id == target.id)
    {
      *current = session.clone();
    }
    if let Some(view) = self
      .view
      .as_mut()
      .filter(|view| view.session_id == target.id)
    {
      view.session_name.clone_from(&session.name);
      // Fetch the committed revision so a delayed older snapshot cannot undo
      // this name. A failed observation does not replay the successful rename.
      match self.get_view(&target.id).await {
        Ok(view) => {
          if let Err(error) = self.adopt_view(view).await {
            self.notice(format!(
              "Renamed session {}; could not refresh view: {error}",
              session.name
            ));
          }
        }
        Err(error) => self.notice(format!(
          "Renamed session {}; could not refresh view: {error}",
          session.name
        )),
      }
    }
    Ok(())
  }

  async fn terminate_session(&mut self, target: &Target, return_to_picker: bool) -> Result<()> {
    if self.read_only {
      return Err("This attachment is read only".into());
    }
    self.maintenance.cancel_refresh();
    timeout(Duration::from_secs(5), async {
      Ok::<_, crate::Error>(
        self
          .session_client()
          .await?
          .terminate(SessionId(target.id.clone()))
          .await?,
      )
    })
    .await??;
    let active = self
      .view
      .as_ref()
      .is_some_and(|view| view.session_id == target.id);
    let mut archive_error = None;
    if active {
      self.ended = Some("Session terminated".into());
      archive_error = self.save_archive().err();
      self.detach().await;
      self.view = None;
      self.selected_id.clear();
      self.focused.clear();
      self.copies.clear();
      self.archived_panes.clear();
      self.ended = None;
    }
    self
      .sessions
      .retain(|session| session.session_id != target.id);
    self.retain_navigation();
    if active || return_to_picker {
      self.open_sessions();
    } else {
      self.overlay = Overlay::None;
    }
    let mut message = format!(
      "Terminated session {} ({})",
      target.name,
      short_id(&target.id)
    );
    if let Some(error) = archive_error {
      write!(message, "; could not save archive: {error}").expect("writing a string cannot fail");
    }
    self.notice(message);
    Ok(())
  }

  fn begin_session_name(&mut self, target: Option<Target>, selected: Option<String>) {
    let mut input = Prompt::default();
    input.open_text(target.as_ref().map_or("", |target| target.name.as_str()));
    self.overlay = Overlay::SessionName(Box::new(NameEntry {
      target,
      selected,
      input,
    }));
  }

  pub(super) async fn session_overlay_key(&mut self, key: KeyEvent) -> Result<Option<bool>> {
    if matches!(self.overlay, Overlay::SessionName(_)) {
      return self.session_name_key(key).await.map(Some);
    }
    if let Overlay::KillSession {
      target,
      return_to_picker,
    } = &self.overlay
    {
      let target = target.clone();
      let return_to_picker = *return_to_picker;
      if return_to_picker {
        self.overlay = Overlay::Sessions(Some(target.id.clone()));
      } else {
        self.overlay = Overlay::None;
      }
      if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
        self.terminate_session(&target, return_to_picker).await?;
      }
      return Ok(Some(false));
    }
    let Overlay::Sessions(selected) = &self.overlay else {
      return Ok(None);
    };
    let selected = selected.clone();
    if !key.modifiers.is_empty() && key.modifiers != KeyModifiers::SHIFT {
      return Ok(Some(false));
    }
    match key.code {
      KeyCode::Up | KeyCode::Down => {
        let index = self.picker_index(selected.as_deref());
        let index = if key.code == KeyCode::Up {
          index.map_or_else(
            || self.sessions.len().saturating_sub(1),
            |index| index.saturating_sub(1),
          )
        } else {
          index.map_or(0, |index| {
            (index + 1).min(self.sessions.len().saturating_sub(1))
          })
        };
        self.overlay = Overlay::Sessions(
          self
            .sessions
            .get(index)
            .map(|session| session.session_id.clone()),
        );
      }
      KeyCode::Enter => {
        if let Some(id) = selected.as_deref().filter(|id| {
          self
            .sessions
            .iter()
            .any(|session| session.session_id == *id)
        }) {
          // Resolve on the server before changing the current attachment.
          // Unlike startup selection, a disappeared picker target stays here.
          self.select(id).await?;
          self.message_until = Instant::now();
        } else {
          self.notice("Selected session no longer exists; choose another session".into());
        }
      }
      KeyCode::Char('n' | 'r' | 'x') if self.read_only => {
        self.notice("This attachment is read only".into());
      }
      KeyCode::Char('n') => self.begin_session_name(None, selected),
      KeyCode::Char('r' | 'x') => {
        if let Some(target) = self
          .sessions
          .iter()
          .find(|session| Some(&session.session_id) == selected.as_ref())
          .map(Target::from)
        {
          if key.code == KeyCode::Char('r') {
            self.begin_session_name(Some(target), selected);
          } else {
            self.overlay = Overlay::KillSession {
              target,
              return_to_picker: true,
            };
          }
        } else {
          self.notice("Selected session no longer exists; choose another session".into());
        }
      }
      KeyCode::Esc => {
        if self.view.is_none() || self.ended.is_some() {
          return Ok(Some(true));
        }
        self.overlay = Overlay::None;
      }
      _ => {}
    }
    Ok(Some(false))
  }

  async fn session_name_key(&mut self, key: KeyEvent) -> Result<bool> {
    let Overlay::SessionName(entry) = &mut self.overlay else {
      unreachable!()
    };
    let event = entry.input.key(key);
    match event {
      PromptEvent::Stay => {}
      PromptEvent::Closed => self.overlay = Overlay::Sessions(entry.selected.clone()),
      PromptEvent::Submit(name) => {
        let target = entry.target.clone();
        let selected = entry.selected.clone();
        self.message_until = Instant::now();
        let result = if target.is_some() && name.trim().is_empty() {
          Err("Session name cannot be empty".into())
        } else if let Some(target) = &target {
          self.rename_session(target, name.clone()).await
        } else {
          self.create_named(Some(name.clone())).await
        };
        match result {
          Ok(()) => {
            if target.is_some() {
              self.overlay = Overlay::Sessions(selected);
            }
          }
          Err(error) => {
            self.begin_session_name(target, selected);
            if let Overlay::SessionName(entry) = &mut self.overlay {
              entry.input.open_text(&name);
            }
            self.notice(error.to_string());
          }
        }
      }
    }
    Ok(false)
  }

  pub(super) fn session_overlay_lines(&self) -> Vec<String> {
    let notice = (self.notice_kind == NoticeKind::Action && Instant::now() < self.message_until)
      .then(|| self.message.clone());
    let mut lines = match &self.overlay {
      Overlay::Sessions(selected) => {
        let host = self.transport.map_or("local", crate::Transport::label);
        let mut lines = vec![
          format!("Sessions — {host}"),
          "↑/↓ choose, Enter opens, n new, r rename, x terminate, Esc returns".into(),
        ];
        if self.sessions.is_empty() {
          lines.push(if self.read_only {
            "No running sessions. Esc detaches.".into()
          } else {
            "No running sessions. n creates one; Esc detaches.".into()
          });
        } else if self.picker_index(selected.as_deref()).is_none() {
          lines.push("Selected session unavailable — arrows choose another".into());
        }
        if self.read_only {
          lines.push("Read only — session management is disabled".into());
        }
        if let Some(notice) = &notice {
          lines.push(notice.clone());
        }
        let capacity = usize::from(self.size.1.saturating_sub(1))
          .saturating_sub(lines.len())
          .max(1);
        let index = self.picker_index(selected.as_deref());
        let start = index.unwrap_or(0).saturating_sub(capacity - 1);
        lines.extend(
          self
            .sessions
            .iter()
            .enumerate()
            .skip(start)
            .take(capacity)
            .map(|(i, session)| {
              format!(
                "{} {}{} ({}) running, created {} ago",
                if Some(i) == index { ">" } else { " " },
                session.name,
                if self
                  .view
                  .as_ref()
                  .is_some_and(|view| view.session_id == session.session_id)
                {
                  " [current]"
                } else {
                  ""
                },
                short_id(&session.session_id),
                age(session.created_at_ms)
              )
            }),
        );
        lines
      }
      Overlay::SessionName(entry) => vec![entry.target.as_ref().map_or_else(
        || "New session name (blank = automatic) — Enter creates, Esc returns".into(),
        |target| {
          format!(
            "Rename {} ({}) — Enter saves, Esc returns",
            target.name,
            short_id(&target.id)
          )
        },
      )],
      Overlay::KillSession { target, .. } => vec![
        format!(
          "Terminate session {} ({})?",
          target.name,
          short_id(&target.id)
        ),
        "This stops ALL panes and disconnects every attached client.".into(),
        "y confirms; any other key cancels".into(),
      ],
      _ => Vec::new(),
    };
    if !matches!(self.overlay, Overlay::Sessions(_))
      && let Some(notice) = notice
    {
      lines.push(notice);
    }
    lines
  }
}

fn short_id(id: &str) -> &str {
  let end = id
    .char_indices()
    .nth(8)
    .map_or(id.len(), |(index, _)| index);
  &id[..end]
}

fn age(created_at_ms: u64) -> String {
  let now = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis();
  let seconds = now.saturating_sub(u128::from(created_at_ms)) / 1_000;
  if seconds < 60 {
    format!("{seconds}s")
  } else if seconds < 3_600 {
    format!("{}m", seconds / 60)
  } else if seconds < 86_400 {
    format!("{}h", seconds / 3_600)
  } else {
    format!("{}d", seconds / 86_400)
  }
}

#[cfg(all(test, unix))]
#[path = "sessions_tests.rs"]
mod tests;
