use ctmux_proto::{
  MAX_ENCODED_HISTORY_BYTES, TerminalHistoryRow, normalize_history_rows,
  retained_history_text_bytes, validate_history_row,
};
use sha2::{Digest, Sha256};
use std::io;
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

/// Immutable physical rows captured with a checkpoint, independent of its cursor.
#[derive(Debug, Default)]
pub(crate) struct PhysicalHistory {
  rows: Vec<TerminalHistoryRow>,
  lines: Vec<String>,
  retained_text_bytes: u64,
  encoded: OnceLock<Result<EncodedHistory, (io::ErrorKind, String)>>,
  legacy_encoded: OnceLock<Result<EncodedHistory, (io::ErrorKind, String)>>,
}

#[derive(Debug)]
pub(crate) struct EncodedHistory {
  pub data: Arc<Vec<u8>>,
  pub content_hash: String,
  pub first_row: usize,
}

impl PhysicalHistory {
  pub fn new(rows: Vec<TerminalHistoryRow>) -> Self {
    let lines = normalize_history_rows(&rows);
    let retained_text_bytes = retained_history_text_bytes(&rows);
    Self {
      rows,
      lines,
      retained_text_bytes,
      encoded: OnceLock::new(),
      legacy_encoded: OnceLock::new(),
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

  pub fn retained_text_bytes(&self) -> u64 {
    self.retained_text_bytes
  }

  pub fn encoded(&self, styled: bool) -> io::Result<&EncodedHistory> {
    let encoded = if styled {
      &self.encoded
    } else {
      &self.legacy_encoded
    };
    encoded
      .get_or_init(|| {
        self
          .encode(styled)
          .map_err(|error| (error.kind(), error.to_string()))
      })
      .as_ref()
      .map_err(|(kind, message)| io::Error::new(*kind, message.clone()))
  }

  fn encode(&self, styled: bool) -> io::Result<EncodedHistory> {
    #[derive(serde::Serialize)]
    struct PlainRow<'a> {
      text: &'a str,
      wrapped: bool,
    }
    // Styles can cost substantially more than their text. Keep the newest
    // contiguous suffix without making an otherwise usable attach fail. The
    // legacy encoding retains its original rows and exact byte representation.
    let mut records = Vec::new();
    let mut total_bytes = 0_usize;
    let mut first_row = self.rows.len();
    for (index, row) in self.rows.iter().enumerate().rev() {
      validate_history_row(row)?;
      let mut record = if styled {
        serde_json::to_vec(row).map_err(io::Error::other)?
      } else {
        serde_json::to_vec(&PlainRow {
          text: &row.text,
          wrapped: row.wrapped,
        })
        .map_err(io::Error::other)?
      };
      record.push(b'\n');
      if total_bytes.saturating_add(record.len()) > MAX_ENCODED_HISTORY_BYTES {
        if styled {
          break;
        }
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "history snapshot exceeds its byte bound",
        ));
      }
      first_row = index;
      total_bytes += record.len();
      records.push(record);
    }
    let mut data = Vec::with_capacity(total_bytes);
    for record in records.into_iter().rev() {
      data.extend(record);
    }
    Ok(EncodedHistory {
      content_hash: format!("{:x}", Sha256::digest(&data)),
      data: Arc::new(data),
      first_row,
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
      styles: Vec::new(),
    }];
    let mut current = Arc::new(PhysicalHistory::new(rows.clone()));
    let captured = Arc::clone(&current);
    let original_data = Arc::clone(&captured.encoded(true).unwrap().data);
    PhysicalHistory::replace(&mut current, rows);
    assert!(Arc::ptr_eq(&current, &captured));
    assert!(Arc::ptr_eq(
      &current.encoded(true).unwrap().data,
      &original_data
    ));
    PhysicalHistory::replace(
      &mut current,
      vec![
        TerminalHistoryRow {
          text: "ab".into(),
          wrapped: true,
          styles: Vec::new(),
        },
        TerminalHistoryRow {
          text: "cd".into(),
          wrapped: false,
          styles: Vec::new(),
        },
      ],
    );
    assert_eq!(current.lines(), captured.lines());
    assert!(!Arc::ptr_eq(&current, &captured));
    assert_ne!(
      current.encoded(true).unwrap().content_hash,
      captured.encoded(true).unwrap().content_hash
    );
    assert_eq!(&*captured.encoded(true).unwrap().data, &*original_data);
    assert_eq!(&*original_data, b"{\"text\":\"abcd\",\"wrapped\":false}\n");
  }

  #[test]
  fn encoding_matches_exact_jsonl_hash_and_is_shared_between_threads() {
    let rows = vec![TerminalHistoryRow {
      text: "界\"\\  ".into(),
      wrapped: true,
      styles: Vec::new(),
    }];
    let history = Arc::new(PhysicalHistory::new(rows.clone()));
    let expected = [serde_json::to_vec(&rows[0]).unwrap(), vec![b'\n']].concat();
    let concurrent = Arc::clone(&history);
    let background =
      std::thread::spawn(move || Arc::clone(&concurrent.encoded(true).unwrap().data));
    let encoded = history.encoded(true).unwrap();
    assert_eq!(&*encoded.data, &expected);
    assert_eq!(
      encoded.content_hash,
      format!("{:x}", Sha256::digest(&expected))
    );
    assert!(Arc::ptr_eq(&encoded.data, &background.join().unwrap()));
  }

  #[test]
  fn styled_rows_have_distinct_cached_bytes_without_changing_legacy_encoding() {
    use ctmux_proto::{TerminalColor, TerminalHistoryStyleSpan, TerminalStyle};
    let row = TerminalHistoryRow {
      text: "red".into(),
      wrapped: false,
      styles: vec![TerminalHistoryStyleSpan {
        start: 0,
        length: 3,
        style: TerminalStyle {
          foreground: Some(TerminalColor::Indexed { index: 1 }),
          ..TerminalStyle::default()
        },
      }],
    };
    let mut history = Arc::new(PhysicalHistory::new(vec![row.clone()]));
    let original = Arc::clone(&history);
    let legacy = Arc::clone(&history.encoded(false).unwrap().data);
    let styled = Arc::clone(&history.encoded(true).unwrap().data);
    assert_eq!(&*legacy, b"{\"text\":\"red\",\"wrapped\":false}\n");
    assert_eq!(
      serde_json::from_slice::<TerminalHistoryRow>(&styled).unwrap(),
      row
    );
    assert_ne!(
      history.encoded(false).unwrap().content_hash,
      history.encoded(true).unwrap().content_hash
    );
    assert!(Arc::ptr_eq(&legacy, &history.encoded(false).unwrap().data));
    let mut recolored = row;
    recolored.styles[0].style.foreground = Some(TerminalColor::Indexed { index: 2 });
    PhysicalHistory::replace(&mut history, vec![recolored]);
    assert_eq!(history.lines(), original.lines());
    assert_eq!(history.encoded(false).unwrap().data, legacy);
    assert_ne!(history.encoded(true).unwrap().data, styled);
  }
}
