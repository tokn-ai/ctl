//! Live overlay messages. Points are fractions of the selected source's image.
use serde::{Deserialize, Serialize};

pub const RECORD_LIMIT: usize = 64 * 1024;
pub const DEFAULT_TURN_MS: u64 = 10_000;
pub const MAX_TURN_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnError {
  pub code: String,
  pub owner_id: Option<String>,
}

/// A host-journaled failure, correlated with the same public ID as a successful action.
#[derive(Debug)]
pub struct ActionFailure {
  pub action_id: String,
  pub source: std::io::Error,
}
impl std::fmt::Display for ActionFailure {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    self.source.fmt(f)
  }
}
impl std::error::Error for ActionFailure {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    Some(&self.source)
  }
}

impl std::fmt::Display for TurnError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match (&*self.code, &self.owner_id) {
      ("busy", Some(owner)) => write!(f, "desktop input is busy (participant {owner})"),
      ("stale_turn", _) => {
        f.write_str("the turn is expired, released or belongs to another connection")
      }
      _ => f.write_str(&self.code),
    }
  }
}

impl std::error::Error for TurnError {}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Participant {
  pub participant_id: String,
  pub agent_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum LiveAction {
  Move { point: [f32; 2] },
  Click { point: [f32; 2] },
  Drag { from: [f32; 2], to: [f32; 2] },
  Scroll { point: [f32; 2] },
  PointerDown { point: [f32; 2] },
  PointerUp { point: [f32; 2] },
  PressKeys {},
  TypeText {},
  Invoke {},
  SetText {},
}

impl LiveAction {
  pub fn valid(&self) -> bool {
    let valid = |p: &[f32; 2]| p.iter().all(|value| (0.0..=1.0).contains(value));
    match self {
      Self::Move { point }
      | Self::Click { point }
      | Self::Scroll { point }
      | Self::PointerDown { point }
      | Self::PointerUp { point } => valid(point),
      Self::Drag { from, to } => valid(from) && valid(to),
      _ => true,
    }
  }

  pub fn operation(&self) -> &'static str {
    match self {
      Self::Move { .. } => "move",
      Self::Click { .. } => "click",
      Self::Drag { .. } => "drag",
      Self::Scroll { .. } => "scroll",
      Self::PointerDown { .. } => "pointer_down",
      Self::PointerUp { .. } => "pointer_up",
      Self::PressKeys {} => "press_keys",
      Self::TypeText {} => "type_text",
      Self::Invoke {} => "invoke",
      Self::SetText {} => "set_text",
    }
  }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LiveEvent {
  Snapshot {
    self_id: String,
    participants: Vec<Participant>,
  },
  Joined {
    participant: Participant,
  },
  Left {
    participant_id: String,
  },
  Action {
    participant_id: String,
    action: LiveAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action_id: Option<String>,
  },
  Turn {
    holder: Option<Participant>,
    expires_at_ms: Option<u64>,
  },
}

impl LiveEvent {
  pub fn valid(&self) -> bool {
    let id = |s: &str| {
      !s.is_empty()
        && s.len() <= 64
        && s
          .bytes()
          .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    };
    let peer = |p: &Participant| id(&p.participant_id) && crate::valid_agent_id(&p.agent_id);
    match self {
      Self::Snapshot {
        self_id,
        participants,
      } => {
        let mut seen = std::collections::HashSet::new();
        participants
          .iter()
          .all(|p| peer(p) && seen.insert(&p.participant_id))
          && participants.iter().any(|p| p.participant_id == *self_id)
      }
      Self::Joined { participant } => peer(participant),
      Self::Left { participant_id } => id(participant_id),
      Self::Action {
        participant_id,
        action,
        action_id,
      } => {
        id(participant_id)
          && action.valid()
          && action_id
            .as_ref()
            .is_none_or(|id| uuid::Uuid::try_parse(id).is_ok_and(|value| value.to_string() == *id))
      }
      Self::Turn {
        holder,
        expires_at_ms,
      } => holder.as_ref().is_none_or(peer) && holder.is_some() == expires_at_ms.is_some(),
    }
  }
}
