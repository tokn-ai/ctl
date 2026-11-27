//! One controller connection. The daemon makes the admission decision, journals
//! it, runs the selected platform worker, and publishes the confirmed result.
use super::{Command, Membership, Reply};
use crate::{
  activity::{Journal, Outcome},
  daemon::{invalid, read_json, write_json},
  desktop_input::{DesktopDriver, InputRequest},
};
use ctream_proto::{
  Target,
  live::{MAX_TURN_MS, TurnError},
};
use std::{
  io,
  sync::{Arc, Mutex},
  time::Duration,
};
use tokio::{
  io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite},
  task::spawn_blocking,
  time::Instant,
};

// A legal 64 KiB public SDK request plus the private transport envelope.
const COMMAND_LIMIT: usize = 2 * 65536;

struct Turn {
  token: String,
  reference: String,
  expires: Instant,
}

struct Controller {
  member: Membership,
  journal: Journal,
  driver: DesktopDriver,
  target: Target,
  turn: Option<Turn>,
  held_until: Option<Instant>,
  expired_hold: bool,
}

impl Controller {
  fn new(member: Membership) -> io::Result<Self> {
    let target = Target::parse(&member.room.target).map_err(io::Error::other)?;
    let driver = DesktopDriver::new(
      target,
      member.room.window.clone(),
      member.key.clone().expect("registered controller"),
    );
    Ok(Self {
      journal: member.rooms.journal.for_session(&member.id)?,
      member,
      driver,
      target,
      turn: None,
      held_until: None,
      expired_hold: false,
    })
  }

  fn deadline(&self) -> Option<Instant> {
    self
      .held_until
      .into_iter()
      .chain(self.turn.as_ref().map(|t| t.expires))
      .min()
  }

  fn expire(&mut self) -> io::Result<()> {
    if self.deadline().is_some_and(|t| t <= Instant::now()) {
      let held = self.driver.raw_held();
      self.driver.reset_raw()?;
      self.held_until = None;
      self.expired_hold |= held && self.turn.is_none();
      if self
        .turn
        .as_ref()
        .is_some_and(|t| t.expires <= Instant::now())
      {
        self.turn = None;
      }
    }
    Ok(())
  }

  fn handle(&mut self, command: Command) -> io::Result<Reply> {
    self.expire()?;
    match command {
      Command::Acquire { ttl_ms } => {
        let start = Instant::now();
        let reply = self.member.command(Command::Acquire { ttl_ms });
        if let Reply::Ok { value: Some(token) } = &reply {
          self.turn = Some(Turn {
            token: token.clone(),
            reference: uuid::Uuid::new_v4().to_string(),
            expires: start + Duration::from_millis(ttl_ms),
          });
          self.expired_hold = false;
        }
        Ok(reply)
      }
      Command::Release { turn_id } => {
        if self.turn.as_ref().is_some_and(|t| t.token == turn_id) {
          if self.driver.raw_held() {
            self.driver.reset_raw()?;
          }
          self.held_until = None;
          self.turn = None;
        }
        Ok(self.member.command(Command::Release { turn_id }))
      }
      Command::PrepareRaw { picker, lease } => {
        let header = self
          .driver
          .raw_header(picker.as_deref(), lease.as_deref())?;
        Ok(Reply::value(header))
      }
      Command::Execute { turn_id, request } => self.execute(turn_id, request),
      #[cfg(test)]
      Command::Begin { .. } | Command::Finish {} => Ok(self.member.command(command)),
      _ => Ok(Reply::invalid(
        "controller requires a complete input request",
      )),
    }
  }

  fn execute(&mut self, turn_id: Option<String>, request: InputRequest) -> io::Result<Reply> {
    let operation = request.operation();
    let label = &self.member.participant.agent_id;
    let window = self.member.room.window.as_deref();
    let turn_ref = self
      .turn
      .as_ref()
      .filter(|t| Some(&t.token) == turn_id.as_ref())
      .map(|t| t.reference.as_str());
    let admission = if std::mem::take(&mut self.expired_hold) {
      Reply::Conflict {
        code: "stale_turn".into(),
        owner_id: None,
      }
    } else {
      self.member.admit(turn_id.clone())
    };
    match admission {
      Reply::Conflict { code, owner_id } => {
        let conflict = TurnError { code, owner_id };
        let action_id =
          self
            .journal
            .refuse(label, self.target, window, operation, turn_ref, &conflict)?;
        return Ok(Reply::ActionFailed {
          action_id,
          failure: crate::daemon::authority::Failure::Conflict(conflict),
        });
      }
      Reply::Ok { .. } => {}
      other => return Ok(other),
    }
    let result = (|| {
      let action_id = self
        .journal
        .start(label, self.target, window, operation, turn_ref)?;
      let overlay = self.driver.overlay(&request);
      let result = self.driver.execute(request);
      let outcome = Outcome::from_result(&result);
      if self.driver.raw_held() {
        self
          .held_until
          .get_or_insert_with(|| Instant::now() + Duration::from_millis(MAX_TURN_MS));
      } else {
        self.held_until = None;
      }
      if let Err(error) = self.journal.finish(&action_id, outcome) {
        // The pre-dispatch unknown row remains durable. Preserve its ID even
        // when the final SQLite transaction cannot commit.
        let uncertain = ctream_driver::UnconfirmedDelivery::error(io::Error::other(format!(
          "input outcome could not be recorded: {error}"
        )));
        return Ok(Reply::action_error(action_id, uncertain));
      }
      match result {
        Ok(()) => {
          if let Some(action) = overlay.filter(|action| action.valid()) {
            self
              .member
              .publish_recorded(action, Some(action_id.clone()))?;
          }
          Ok(Reply::value(action_id))
        }
        Err(error) => Ok(Reply::action_error(action_id, error)),
      }
    })();
    self.member.command(Command::Finish {});
    result
  }
}

async fn blocking<T: Send + 'static>(
  work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
  spawn_blocking(work).await.map_err(io::Error::other)?
}

pub(super) async fn serve(
  member: Membership,
  reader: &mut (impl AsyncBufRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<()> {
  let rooms = member.rooms.clone();
  let key = member.key.clone().expect("registered controller");
  // Only worker processes launched by the daemon get a delegation key.
  #[cfg(test)]
  let ready = Reply::value(key.clone());
  #[cfg(not(test))]
  let ready = Reply::done();
  let cancellation = member
    .attachment
    .as_ref()
    .map(|attachment| attachment.revoked.clone());
  let revoked = async {
    match cancellation {
      Some(signal) => signal.notified().await,
      None => std::future::pending().await,
    }
  };
  tokio::pin!(revoked);
  let controller = Arc::new(Mutex::new(Controller::new(member)?));
  let mut close_requested = false;
  let result = async {
    write_json(writer, &ready).await?;
    'commands: loop {
      let command = {
        let packet = read_json::<Command>(reader, COMMAND_LIMIT);
        tokio::pin!(packet);
        loop {
          let deadline = controller.lock().unwrap().deadline();
          let expiry = async {
            match deadline {
              Some(at) => tokio::time::sleep_until(at).await,
              None => std::future::pending().await,
            }
          };
          tokio::select! {
            biased;
            _ = &mut revoked => break 'commands,
            _ = expiry => {
              let state = controller.clone();
              blocking(move || state.lock().unwrap().expire()).await?;
            }
            packet = &mut packet => {
              let Some(command) = packet? else { break 'commands };
              break command;
            }
          }
        }
      };
      if matches!(command, Command::Close {}) {
        close_requested = true;
        break;
      }
      // Private control lanes are request/response: until the reply, the next
      // read can only be an EOF, transport failure, or a protocol violation.
      // Wake queued admission immediately. Work already admitted retains its
      // capability and finishes its driver call and durable audit.
      let state = controller.clone();
      let work = blocking(move || state.lock().unwrap().handle(command));
      tokio::pin!(work);
      let disconnected = async {
        tokio::select! {
          biased;
          _ = &mut revoked => Ok(()),
          result = reader.fill_buf() => match result? {
            [] => Ok(()),
            _ => Err(invalid("controller sent input before receiving its reply")),
          },
        }
      };
      let response = tokio::select! {
        biased;
        reason = disconnected => {
          rooms.stop_admitting(&key);
          let _ = work.await;
          reason?;
          break;
        }
        result = &mut work => result.unwrap_or_else(|error| Reply::invalid(&error.to_string())),
      };
      tokio::select! {
        biased;
        _ = &mut revoked => break,
        result = write_json(writer, &response) => result?,
      }
    }
    Ok(())
  }
  .await;
  // Driver shutdown may wait on a delegated worker connected to this same
  // reactor. Drain it on a blocking thread before acknowledging Close.
  let cleanup = blocking(move || {
    let mut owner = Arc::try_unwrap(controller)
      .map_err(|_| invalid("controller still executing"))?
      .into_inner()
      .unwrap();
    owner.driver.reset_raw()
  })
  .await;
  result.and(cleanup)?;
  if close_requested {
    write_json(writer, &Reply::done()).await?;
  }
  Ok(())
}
