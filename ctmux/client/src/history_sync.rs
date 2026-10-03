//! Assemble one immutable transfer of a replaceable remote history window.
//! The transfer position never advances the live renderer or its resume cursor.
use crate::{AttachmentEvent, ClientError};
use ctmux_proto::{
  ClientMessage, MAX_HISTORY_PAGE_BYTES, TerminalCheckpoint, TerminalHistoryManifest,
  TerminalHistoryRow, TerminalHistorySnapshot, normalize_history_rows,
};
use sha2::{Digest, Sha256};

const MAX_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) struct HistoryTransfer {
  manifest: TerminalHistoryManifest,
  checkpoint: TerminalCheckpoint,
  history: TerminalHistorySnapshot,
  history_gap: bool,
  bytes: Vec<u8>,
}

fn invalid(message: &str) -> ClientError {
  ClientError::InvalidHistorySnapshot(message.into())
}

impl HistoryTransfer {
  pub(crate) fn new(
    manifest: TerminalHistoryManifest,
    checkpoint: TerminalCheckpoint,
    history: TerminalHistorySnapshot,
    history_gap: bool,
  ) -> Result<Self, ClientError> {
    if manifest.snapshot_id.is_empty()
      || manifest.sequence != checkpoint.sequence
      || manifest.generation != history.generation
      || manifest.revision != history.revision
      || manifest.total_bytes > MAX_SNAPSHOT_BYTES
      || manifest.total_rows > manifest.scrollback_limit
      || manifest.scrollback_limit > 10_000
      || manifest.first_line.checked_add(history.lines.len() as u64) != Some(manifest.total_lines)
      || manifest.content_hash.len() != 64
      || !manifest
        .content_hash
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
    {
      return Err(invalid("history manifest does not match its checkpoint"));
    }
    Ok(Self {
      manifest,
      checkpoint,
      history,
      history_gap,
      bytes: Vec::new(),
    })
  }

  pub(crate) fn snapshot_id(&self) -> &str {
    &self.manifest.snapshot_id
  }

  pub(crate) fn sequence(&self) -> u64 {
    self.checkpoint.sequence
  }

  pub(crate) fn request(&self) -> ClientMessage {
    ClientMessage::HistoryRequest {
      snapshot_id: self.manifest.snapshot_id.clone(),
      offset: self.bytes.len() as u64,
      max_bytes: MAX_HISTORY_PAGE_BYTES as u64,
    }
  }

  pub(crate) fn is_complete(&self) -> bool {
    self.bytes.len() as u64 == self.manifest.total_bytes
  }

  pub(crate) fn accept_page(
    &mut self,
    offset: u64,
    data: &[u8],
    next_offset: Option<u64>,
  ) -> Result<(), ClientError> {
    let end = offset.checked_add(data.len() as u64);
    if offset != self.bytes.len() as u64
      || data.len() > MAX_HISTORY_PAGE_BYTES
      || end.is_none_or(|end| end > self.manifest.total_bytes)
      || (data.is_empty() && !self.is_complete())
      || next_offset != end.filter(|end| *end < self.manifest.total_bytes)
    {
      return Err(invalid(
        "history page is outside its requested snapshot range",
      ));
    }
    self.bytes.extend_from_slice(data);
    Ok(())
  }

  pub(crate) fn finish(mut self) -> Result<AttachmentEvent, ClientError> {
    if !self.is_complete()
      || format!("{:x}", Sha256::digest(&self.bytes)) != self.manifest.content_hash
    {
      return Err(invalid("history snapshot checksum does not match"));
    }
    let mut rows = Vec::new();
    for record in self.bytes.split_inclusive(|byte| *byte == b'\n') {
      if record.last() != Some(&b'\n') {
        return Err(invalid("history snapshot contains an incomplete record"));
      }
      let row: TerminalHistoryRow = serde_json::from_slice(&record[..record.len() - 1])
        .map_err(|error| ClientError::InvalidHistorySnapshot(error.to_string()))?;
      if row.text.chars().any(char::is_control) {
        return Err(invalid("history row contains terminal control characters"));
      }
      rows.push(row);
    }
    if rows.len() as u64 != self.manifest.total_rows {
      return Err(invalid("history snapshot row count does not match"));
    }
    self.history.lines = normalize_history_rows(&rows);
    if self.history.lines.len() as u64 != self.manifest.total_lines {
      return Err(invalid("history snapshot line count does not match"));
    }
    self.history.retained_bytes = self
      .history
      .lines
      .iter()
      .map(|line| line.len() as u64 + 1)
      .sum();
    self.history.truncated = self.manifest.truncated;
    Ok(AttachmentEvent::HistorySynced {
      snapshot_id: self.manifest.snapshot_id,
      checkpoint: self.checkpoint,
      history: self.history,
      rows,
      scrollback_limit: self.manifest.scrollback_limit,
      history_gap: self.history_gap || self.manifest.truncated,
    })
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;

  pub(crate) fn fixture(
    sequence: u64,
    snapshot_id: &str,
    rows: &[TerminalHistoryRow],
  ) -> (
    TerminalHistoryManifest,
    TerminalCheckpoint,
    TerminalHistorySnapshot,
    Vec<u8>,
  ) {
    let bytes: Vec<_> = rows
      .iter()
      .flat_map(|row| {
        let mut record = serde_json::to_vec(row).unwrap();
        record.push(b'\n');
        record
      })
      .collect();
    let lines = normalize_history_rows(rows);
    let history = TerminalHistorySnapshot {
      format: ctmux_proto::TERMINAL_HISTORY_FORMAT.into(),
      format_version: ctmux_proto::TERMINAL_HISTORY_FORMAT_VERSION,
      sequence,
      generation: 0,
      revision: 1,
      retained_bytes: lines.iter().map(|line| line.len() as u64 + 1).sum(),
      lines,
      truncated: false,
    };
    let checkpoint = TerminalCheckpoint {
      format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
      sequence,
      terminal_size: ctmux_proto::TerminalSize::default(),
      payload: Vec::new(),
      input_prefix: Vec::new(),
    };
    let manifest = TerminalHistoryManifest {
      snapshot_id: snapshot_id.into(),
      sequence,
      generation: history.generation,
      revision: history.revision,
      total_rows: rows.len() as u64,
      total_bytes: bytes.len() as u64,
      total_lines: history.lines.len() as u64,
      first_line: 0,
      truncated: false,
      content_hash: format!("{:x}", Sha256::digest(&bytes)),
      scrollback_limit: 10_000,
    };
    (manifest, checkpoint, history, bytes)
  }

  #[test]
  fn byte_pages_can_split_utf8_and_preserve_repeated_lines() {
    let rows = vec![
      TerminalHistoryRow {
        text: "界".into(),
        wrapped: false,
      },
      TerminalHistoryRow {
        text: "same".into(),
        wrapped: false,
      },
      TerminalHistoryRow {
        text: "same".into(),
        wrapped: false,
      },
      TerminalHistoryRow {
        text: "partial".into(),
        wrapped: true,
      },
    ];
    let (manifest, checkpoint, mut history, bytes) = fixture(9, "snap", &rows);
    history.lines = vec!["same".into()];
    let manifest = TerminalHistoryManifest {
      first_line: 2,
      ..manifest
    };
    let mut transfer = HistoryTransfer::new(manifest, checkpoint, history, false).unwrap();
    for (index, byte) in bytes.iter().enumerate() {
      transfer
        .accept_page(
          index as u64,
          &[*byte],
          (index + 1 < bytes.len()).then_some(index as u64 + 1),
        )
        .unwrap();
    }
    let AttachmentEvent::HistorySynced {
      history,
      rows: actual,
      ..
    } = transfer.finish().unwrap()
    else {
      panic!("expected history")
    };
    assert_eq!(actual, rows);
    assert_eq!(history.lines, ["界", "same", "same"]);
  }

  #[test]
  fn missing_pages_and_bad_hashes_are_rejected() {
    let rows = [TerminalHistoryRow {
      text: "retained".into(),
      wrapped: false,
    }];
    let (manifest, checkpoint, history, bytes) = fixture(9, "snap", &rows);
    let mut transfer =
      HistoryTransfer::new(manifest.clone(), checkpoint.clone(), history.clone(), false).unwrap();
    assert!(transfer.accept_page(1, &bytes, None).is_err());
    assert!(transfer.accept_page(0, &[], None).is_err());
    assert!(
      transfer
        .accept_page(0, &bytes, Some(bytes.len() as u64))
        .is_err()
    );
    let bad = TerminalHistoryManifest {
      content_hash: "0".repeat(64),
      ..manifest
    };
    let mut transfer = HistoryTransfer::new(bad, checkpoint, history, false).unwrap();
    transfer.accept_page(0, &bytes, None).unwrap();
    assert!(transfer.finish().is_err());
  }

  #[test]
  fn empty_snapshot_completes_without_a_page_request() {
    let (manifest, checkpoint, history, _) = fixture(9, "snap", &[]);
    let transfer = HistoryTransfer::new(manifest, checkpoint, history, false).unwrap();
    assert!(transfer.is_complete());
    assert!(
      matches!(transfer.finish().unwrap(), AttachmentEvent::HistorySynced { rows, .. } if rows.is_empty())
    );
  }

  #[test]
  fn checksummed_control_characters_are_rejected() {
    for text in ["\x1b[3J", "\u{009b}3J"] {
      let rows = [TerminalHistoryRow {
        text: text.into(),
        wrapped: false,
      }];
      let (manifest, checkpoint, history, bytes) = fixture(9, "snap", &rows);
      let mut transfer = HistoryTransfer::new(manifest, checkpoint, history, false).unwrap();
      transfer.accept_page(0, &bytes, None).unwrap();
      assert!(transfer.finish().is_err());
    }
  }
}
