use ctmux_proto::{TerminalHistoryRow, normalize_history_rows};
use sha2::{Digest, Sha256};
use std::io;
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

const MAX_ENCODED_HISTORY_BYTES: usize = 16 * 1024 * 1024;

/// Immutable physical rows captured with a checkpoint, independent of its cursor.
#[derive(Debug, Default)]
pub(crate) struct PhysicalHistory {
  rows: Vec<TerminalHistoryRow>,
  lines: Vec<String>,
  encoded: OnceLock<Result<EncodedHistory, (io::ErrorKind, String)>>,
}

#[derive(Debug)]
pub(crate) struct EncodedHistory {
  pub data: Arc<Vec<u8>>,
  pub content_hash: String,
}

impl PhysicalHistory {
  pub fn new(rows: Vec<TerminalHistoryRow>) -> Self {
    let lines = normalize_history_rows(&rows);
    Self {
      rows,
      lines,
      encoded: OnceLock::new(),
    }
  }

  pub fn replace(current: &mut Arc<Self>, rows: Vec<TerminalHistoryRow>) {
    // Logical lines alone do not identify physical wrapping after a resize.
    if current.rows != rows {
      *current = Arc::new(Self::new(rows));
    }
  }

  pub fn lines(&self) -> &[String] {
    &self.lines
  }

  pub fn encoded(&self) -> io::Result<&EncodedHistory> {
    self
      .encoded
      .get_or_init(|| {
        self
          .encode()
          .map_err(|error| (error.kind(), error.to_string()))
      })
      .as_ref()
      .map_err(|(kind, message)| io::Error::new(*kind, message.clone()))
  }

  fn encode(&self) -> io::Result<EncodedHistory> {
    let mut data = Vec::new();
    for row in &self.rows {
      serde_json::to_writer(&mut data, row).map_err(io::Error::other)?;
      data.push(b'\n');
      if data.len() > MAX_ENCODED_HISTORY_BYTES {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "history snapshot exceeds its byte bound",
        ));
      }
    }
    Ok(EncodedHistory {
      content_hash: format!("{:x}", Sha256::digest(&data)),
      data: Arc::new(data),
    })
  }
}

impl Deref for PhysicalHistory {
  type Target = [TerminalHistoryRow];

  fn deref(&self) -> &Self::Target {
    &self.rows
  }
}

impl PartialEq for PhysicalHistory {
  fn eq(&self, other: &Self) -> bool {
    self.rows == other.rows
  }
}

impl Eq for PhysicalHistory {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn identical_rows_share_one_encoding_and_wrapped_changes_replace_it() {
    let rows = vec![TerminalHistoryRow {
      text: "abcd".into(),
      wrapped: false,
    }];
    let mut current = Arc::new(PhysicalHistory::new(rows.clone()));
    let captured = Arc::clone(&current);
    let original_data = Arc::clone(&captured.encoded().unwrap().data);
    PhysicalHistory::replace(&mut current, rows);
    assert!(Arc::ptr_eq(&current, &captured));
    assert!(Arc::ptr_eq(
      &current.encoded().unwrap().data,
      &original_data
    ));
    PhysicalHistory::replace(
      &mut current,
      vec![
        TerminalHistoryRow {
          text: "ab".into(),
          wrapped: true,
        },
        TerminalHistoryRow {
          text: "cd".into(),
          wrapped: false,
        },
      ],
    );
    assert_eq!(current.lines(), captured.lines());
    assert!(!Arc::ptr_eq(&current, &captured));
    assert_ne!(
      current.encoded().unwrap().content_hash,
      captured.encoded().unwrap().content_hash
    );
    assert_eq!(&*captured.encoded().unwrap().data, &*original_data);
    assert_eq!(&*original_data, b"{\"text\":\"abcd\",\"wrapped\":false}\n");
  }

  #[test]
  fn encoding_matches_exact_jsonl_hash_and_is_shared_between_threads() {
    let rows = vec![TerminalHistoryRow {
      text: "界\"\\  ".into(),
      wrapped: true,
    }];
    let history = Arc::new(PhysicalHistory::new(rows.clone()));
    let expected = [serde_json::to_vec(&rows[0]).unwrap(), vec![b'\n']].concat();
    let concurrent = Arc::clone(&history);
    let background = std::thread::spawn(move || Arc::clone(&concurrent.encoded().unwrap().data));
    let encoded = history.encoded().unwrap();
    assert_eq!(&*encoded.data, &expected);
    assert_eq!(
      encoded.content_hash,
      format!("{:x}", Sha256::digest(&expected))
    );
    assert!(Arc::ptr_eq(&encoded.data, &background.join().unwrap()));
  }
}
