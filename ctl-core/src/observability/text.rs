use super::{Component, Context, Event, Lease, Level, Outcome, Record};
use serde::{Serialize, de::DeserializeOwned};
use std::io;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

impl Record {
  /// Render one bounded human-readable log line, with a UTC timestamp.
  ///
  /// # Errors
  /// Returns an error for invalid metadata or an unrepresentable timestamp.
  pub fn log_line(&self) -> io::Result<String> {
    if !self.valid() {
      return Err(io::Error::other("invalid log record"));
    }
    let timestamp =
      OffsetDateTime::from_unix_timestamp_nanos(i128::from(self.timestamp_ms) * 1_000_000)
        .map_err(io::Error::other)?
        .format(&Rfc3339)
        .map_err(io::Error::other)?;
    Ok(format!(
      "{timestamp} {} {} {} {} session={} pane={} attachment={} exit_code={} lease={} duration_ms={} subject={} code={} os={} pid={} run={} operation={} event_id={} schema={}",
      name(self.level).to_ascii_uppercase(),
      name(self.component),
      name(self.event),
      name(self.outcome),
      optional(self.context.session_id),
      optional(self.context.pane_id),
      optional(self.context.attachment_id),
      optional(self.context.exit_code),
      self.context.lease.map_or_else(|| "-".into(), name),
      self.elapsed_ms,
      self.subject_id.as_deref().unwrap_or("-"),
      self.error_code.as_deref().unwrap_or("-"),
      optional(self.os_error),
      self.process_id,
      self.run_id,
      self.operation_id,
      self.event_id,
      self.schema_version,
    ))
  }

  pub(super) fn from_log_line(line: &[u8]) -> Option<Self> {
    if !line.ends_with(b"\n") {
      return None;
    }
    let fields: Vec<_> = std::str::from_utf8(line).ok()?.split_whitespace().collect();
    if fields.len() != 19 {
      return None;
    }
    let timestamp = OffsetDateTime::parse(fields[0], &Rfc3339).ok()?;
    let record = Self {
      timestamp_ms: u64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000).ok()?,
      level: fields[1].parse::<Level>().ok()?,
      component: parse_name::<Component>(fields[2])?,
      event: parse_name::<Event>(fields[3])?,
      outcome: parse_name::<Outcome>(fields[4])?,
      context: Context {
        session_id: parse_optional(field(fields[5], "session")?).ok()?,
        pane_id: parse_optional(field(fields[6], "pane")?).ok()?,
        attachment_id: parse_optional(field(fields[7], "attachment")?).ok()?,
        exit_code: parse_optional(field(fields[8], "exit_code")?).ok()?,
        lease: match field(fields[9], "lease")? {
          "-" => None,
          value => Some(parse_name::<Lease>(value)?),
        },
      },
      elapsed_ms: field(fields[10], "duration_ms")?.parse().ok()?,
      subject_id: optional_text(field(fields[11], "subject")?),
      error_code: optional_text(field(fields[12], "code")?),
      os_error: parse_optional(field(fields[13], "os")?).ok()?,
      process_id: field(fields[14], "pid")?.parse().ok()?,
      run_id: field(fields[15], "run")?.parse().ok()?,
      operation_id: field(fields[16], "operation")?.parse().ok()?,
      event_id: field(fields[17], "event_id")?.parse().ok()?,
      schema_version: field(fields[18], "schema")?.parse().ok()?,
    };
    record.valid().then_some(record)
  }
}

fn name(value: impl Serialize) -> String {
  // All callers are allowlisted unit enums; their serialization cannot fail.
  serde_json::to_value(value)
    .expect("unit enum serializes")
    .as_str()
    .expect("unit enum is a string")
    .to_owned()
}

fn parse_name<T: DeserializeOwned>(value: &str) -> Option<T> {
  serde_json::from_value(serde_json::Value::String(value.into())).ok()
}

fn field<'a>(value: &'a str, key: &str) -> Option<&'a str> {
  let (actual, value) = value.split_once('=')?;
  (actual == key).then_some(value)
}

fn optional<T: std::fmt::Display>(value: Option<T>) -> String {
  value.map_or_else(|| "-".into(), |value| value.to_string())
}

fn parse_optional<T: std::str::FromStr>(value: &str) -> Result<Option<T>, T::Err> {
  if value == "-" {
    Ok(None)
  } else {
    value.parse().map(Some)
  }
}

fn optional_text(value: &str) -> Option<String> {
  (value != "-").then(|| value.into())
}
