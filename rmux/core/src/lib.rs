use rmux_proto::{LeaseKind, LeaseStatus, ServerMessage};
use std::collections::VecDeque;
use thiserror::Error;

/// The input and layout leases held by one attachment after an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentLeases {
  pub input: LeaseStatus,
  pub layout: LeaseStatus,
}

/// Portable, attachment-scoped ownership policy for a terminal session.
///
/// The daemon owns synchronization around this state. The registry itself is
/// intentionally independent of PTYs and transports, so Unix sockets, named
/// pipes, and a future remote gateway share the same ownership rules.
#[derive(Debug, Default)]
pub struct AttachmentLeaseRegistry {
  input_owner: Option<String>,
  layout_owner: Option<String>,
}

impl AttachmentLeaseRegistry {
  /// Claims each requested unheld lease without taking one from another
  /// attachment.
  #[must_use]
  pub fn request_initial(
    &mut self,
    attachment_id: &str,
    request_input_lease: bool,
    request_layout_lease: bool,
  ) -> AttachmentLeases {
    if request_input_lease && self.input_owner.is_none() {
      self.input_owner = Some(attachment_id.into());
    }
    if request_layout_lease && self.layout_owner.is_none() {
      self.layout_owner = Some(attachment_id.into());
    }
    self.attachment_leases(attachment_id)
  }

  /// Claims an unheld lease, or returns its existing state without stealing it.
  #[must_use]
  pub fn acquire(&mut self, attachment_id: &str, lease: LeaseKind) -> LeaseStatus {
    {
      let owner = self.lease_owner_mut(lease);
      if owner.is_none() || owner.as_deref() == Some(attachment_id) {
        *owner = Some(attachment_id.into());
      }
    }
    self.status(attachment_id, lease)
  }

  /// Releases `lease` only when it belongs to `attachment_id`.
  #[must_use]
  pub fn release(&mut self, attachment_id: &str, lease: LeaseKind) -> LeaseStatus {
    {
      let owner = self.lease_owner_mut(lease);
      release_owned_lease(owner, attachment_id);
    }
    self.status(attachment_id, lease)
  }

  /// Releases every lease held by a detached attachment.
  pub fn release_attachment(&mut self, attachment_id: &str) {
    release_owned_lease(&mut self.input_owner, attachment_id);
    release_owned_lease(&mut self.layout_owner, attachment_id);
  }

  /// Returns lease state from the perspective of `attachment_id`.
  #[must_use]
  pub fn status(&self, attachment_id: &str, lease: LeaseKind) -> LeaseStatus {
    let owner = self.lease_owner(lease);
    LeaseStatus {
      held: owner.is_some(),
      owned_by_client: owner == Some(attachment_id),
    }
  }

  #[must_use]
  pub fn attachment_leases(&self, attachment_id: &str) -> AttachmentLeases {
    AttachmentLeases {
      input: self.status(attachment_id, LeaseKind::Input),
      layout: self.status(attachment_id, LeaseKind::Layout),
    }
  }

  fn lease_owner(&self, lease: LeaseKind) -> Option<&str> {
    match lease {
      LeaseKind::Input => self.input_owner.as_deref(),
      LeaseKind::Layout => self.layout_owner.as_deref(),
    }
  }

  fn lease_owner_mut(&mut self, lease: LeaseKind) -> &mut Option<String> {
    match lease {
      LeaseKind::Input => &mut self.input_owner,
      LeaseKind::Layout => &mut self.layout_owner,
    }
  }
}

fn release_owned_lease(owner: &mut Option<String>, attachment_id: &str) {
  if owner.as_deref() == Some(attachment_id) {
    *owner = None;
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputChunk {
  pub sequence_start: u64,
  pub data: Vec<u8>,
}

impl OutputChunk {
  #[must_use]
  pub fn sequence_end(&self) -> u64 {
    self.sequence_start + self.data.len() as u64
  }

  #[must_use]
  pub fn into_server_message(self) -> ServerMessage {
    ServerMessage::Output {
      sequence_start: self.sequence_start,
      sequence_end: self.sequence_end(),
      data: self.data,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalSnapshot {
  pub earliest_sequence: u64,
  pub next_sequence: u64,
  pub replay_from: u64,
  pub history_gap: bool,
  pub chunks: Vec<OutputChunk>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum JournalError {
  #[error("requested sequence {requested} is ahead of the next sequence {next}")]
  SequenceAhead { requested: u64, next: u64 },
}

#[derive(Debug)]
pub struct OutputJournal {
  capacity_bytes: usize,
  retained_bytes: usize,
  next_sequence: u64,
  chunks: VecDeque<OutputChunk>,
}

impl OutputJournal {
  #[must_use]
  pub fn new(capacity_bytes: usize) -> Self {
    Self {
      capacity_bytes: capacity_bytes.max(1),
      retained_bytes: 0,
      next_sequence: 0,
      chunks: VecDeque::new(),
    }
  }

  #[must_use]
  pub fn earliest_sequence(&self) -> u64 {
    self
      .chunks
      .front()
      .map_or(self.next_sequence, |chunk| chunk.sequence_start)
  }

  #[must_use]
  pub fn next_sequence(&self) -> u64 {
    self.next_sequence
  }

  pub fn append(&mut self, data: &[u8]) -> Option<OutputChunk> {
    if data.is_empty() {
      return None;
    }

    let live_chunk = OutputChunk {
      sequence_start: self.next_sequence,
      data: data.to_vec(),
    };
    self.next_sequence = live_chunk.sequence_end();
    self.retained_bytes += live_chunk.data.len();
    self.chunks.push_back(live_chunk.clone());
    self.enforce_capacity();
    Some(live_chunk)
  }

  /// Copies retained output beginning at the requested byte sequence.
  ///
  /// # Errors
  ///
  /// Returns [`JournalError::SequenceAhead`] when the requested sequence is
  /// beyond output produced by the session.
  pub fn snapshot_from(&self, requested: Option<u64>) -> Result<JournalSnapshot, JournalError> {
    if let Some(sequence) = requested
      && sequence > self.next_sequence
    {
      return Err(JournalError::SequenceAhead {
        requested: sequence,
        next: self.next_sequence,
      });
    }

    let earliest_sequence = self.earliest_sequence();
    let requested_sequence = requested.unwrap_or(earliest_sequence);
    let history_gap = requested_sequence < earliest_sequence;
    let replay_from = requested_sequence.max(earliest_sequence);
    let chunks = self
      .chunks
      .iter()
      .filter_map(|chunk| slice_chunk_from(chunk, replay_from))
      .collect();

    Ok(JournalSnapshot {
      earliest_sequence,
      next_sequence: self.next_sequence,
      replay_from,
      history_gap,
      chunks,
    })
  }

  fn enforce_capacity(&mut self) {
    while self.retained_bytes > self.capacity_bytes {
      let excess = self.retained_bytes - self.capacity_bytes;
      let Some(front) = self.chunks.front_mut() else {
        self.retained_bytes = 0;
        return;
      };

      if excess >= front.data.len() {
        self.retained_bytes -= front.data.len();
        self.chunks.pop_front();
      } else {
        front.data.drain(..excess);
        front.sequence_start += excess as u64;
        self.retained_bytes -= excess;
      }
    }
  }
}

fn slice_chunk_from(chunk: &OutputChunk, sequence: u64) -> Option<OutputChunk> {
  if chunk.sequence_end() <= sequence {
    return None;
  }

  if chunk.sequence_start >= sequence {
    return Some(chunk.clone());
  }

  let Ok(offset) = usize::try_from(sequence - chunk.sequence_start) else {
    return None;
  };
  Some(OutputChunk {
    sequence_start: sequence,
    data: chunk.data[offset..].to_vec(),
  })
}

/// Validates a session name for display and safe lookup.
///
/// # Errors
///
/// Returns a description when the name is empty, too long, or contains a byte
/// outside the allowed portable character set.
pub fn validate_session_name(name: &str) -> Result<(), &'static str> {
  if name.is_empty() || name.len() > 64 {
    return Err("session names must contain between 1 and 64 bytes");
  }

  if !name
    .bytes()
    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
  {
    return Err("session names may contain only ASCII letters, digits, '-', '_', and '.'");
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn journal_assigns_byte_sequences() {
    let mut journal = OutputJournal::new(64);
    let first = journal.append(b"abc").unwrap();
    let second = journal.append(b"de").unwrap();

    assert_eq!(first.sequence_start, 0);
    assert_eq!(first.sequence_end(), 3);
    assert_eq!(second.sequence_start, 3);
    assert_eq!(second.sequence_end(), 5);
    assert_eq!(journal.next_sequence(), 5);
  }

  #[test]
  fn journal_evicts_only_the_required_prefix() {
    let mut journal = OutputJournal::new(5);
    journal.append(b"abc");
    journal.append(b"defg");

    let snapshot = journal.snapshot_from(None).unwrap();
    assert_eq!(snapshot.earliest_sequence, 2);
    assert_eq!(snapshot.next_sequence, 7);
    assert_eq!(snapshot.chunks[0].data, b"c");
    assert_eq!(snapshot.chunks[1].data, b"defg");
  }

  #[test]
  fn old_resume_sequence_reports_a_history_gap() {
    let mut journal = OutputJournal::new(4);
    journal.append(b"abcdef");

    let snapshot = journal.snapshot_from(Some(1)).unwrap();
    assert!(snapshot.history_gap);
    assert_eq!(snapshot.replay_from, 2);
    assert_eq!(snapshot.chunks[0].data, b"cdef");
  }

  #[test]
  fn resume_can_begin_inside_a_chunk() {
    let mut journal = OutputJournal::new(64);
    journal.append(b"abcdef");

    let snapshot = journal.snapshot_from(Some(3)).unwrap();
    assert!(!snapshot.history_gap);
    assert_eq!(snapshot.chunks[0].sequence_start, 3);
    assert_eq!(snapshot.chunks[0].data, b"def");
  }

  #[test]
  fn sequence_ahead_is_rejected() {
    let mut journal = OutputJournal::new(64);
    journal.append(b"abc");

    assert_eq!(
      journal.snapshot_from(Some(4)),
      Err(JournalError::SequenceAhead {
        requested: 4,
        next: 3,
      })
    );
  }

  #[test]
  fn validates_safe_session_names() {
    assert!(validate_session_name("work-1.main").is_ok());
    assert!(validate_session_name("").is_err());
    assert!(validate_session_name("contains spaces").is_err());
    assert!(validate_session_name("../socket").is_err());
  }

  #[test]
  fn attachment_leases_do_not_transfer_implicitly() {
    let mut leases = AttachmentLeaseRegistry::default();

    let first = leases.request_initial("first", true, true);
    assert_eq!(
      first,
      AttachmentLeases {
        input: LeaseStatus {
          held: true,
          owned_by_client: true,
        },
        layout: LeaseStatus {
          held: true,
          owned_by_client: true,
        },
      }
    );

    let second = leases.request_initial("second", true, true);
    assert_eq!(
      second,
      AttachmentLeases {
        input: LeaseStatus {
          held: true,
          owned_by_client: false,
        },
        layout: LeaseStatus {
          held: true,
          owned_by_client: false,
        },
      }
    );
    assert_eq!(
      leases.acquire("second", LeaseKind::Input),
      LeaseStatus {
        held: true,
        owned_by_client: false,
      }
    );
  }

  #[test]
  fn attachment_release_makes_only_its_leases_available() {
    let mut leases = AttachmentLeaseRegistry::default();
    let _ = leases.request_initial("input", true, false);
    let _ = leases.request_initial("layout", false, true);

    leases.release_attachment("input");
    assert_eq!(
      leases.acquire("viewer", LeaseKind::Input),
      LeaseStatus {
        held: true,
        owned_by_client: true,
      }
    );
    assert_eq!(
      leases.status("viewer", LeaseKind::Layout),
      LeaseStatus {
        held: true,
        owned_by_client: false,
      }
    );

    assert_eq!(
      leases.release("layout", LeaseKind::Layout),
      LeaseStatus {
        held: false,
        owned_by_client: false,
      }
    );
    assert_eq!(
      leases.acquire("viewer", LeaseKind::Layout),
      LeaseStatus {
        held: true,
        owned_by_client: true,
      }
    );
  }
}
