use clap::Args;
use ctl_core::observability::{Outcome, Stream, user_store};
use std::io;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug, Clone, Copy, Args)]
pub struct Arguments {
  /// Maximum number of newest matching records to display.
  #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=10000))]
  limit: u32,
  /// Show only failures and interrupted operations.
  #[arg(long)]
  failed: bool,
  /// Print records and completeness information as JSON.
  #[arg(long)]
  json: bool,
  /// Show the local history directory without creating it.
  #[arg(long, conflicts_with_all = ["failed", "json"])]
  path: bool,
}

pub fn dispatch(command: &crate::Command) -> Option<io::Result<()>> {
  match command {
    crate::Command::Logs(arguments) => Some(run(*arguments, Stream::Logs)),
    crate::Command::Audit(arguments) => Some(run(*arguments, Stream::Audit)),
    _ => None,
  }
}

pub fn run(arguments: Arguments, stream: Stream) -> io::Result<()> {
  let store = user_store()?;
  if arguments.path {
    println!(
      "{}",
      crate::table::text(&store.directory().display().to_string())
    );
    return Ok(());
  }
  let history = store.read(stream, arguments.limit as usize, arguments.failed)?;
  if arguments.json {
    println!(
      "{}",
      serde_json::to_string(&history).map_err(io::Error::other)?
    );
  } else {
    if history.records.is_empty() {
      println!("No matching history records.");
    } else {
      println!(
        "{}",
        crate::table::format(
          [
            "TIME (UTC)",
            "EVENT",
            "RESULT",
            "SUBJECT",
            "DURATION",
            "DETAIL"
          ],
          history.records.iter().map(|record| {
            let time = OffsetDateTime::from_unix_timestamp_nanos(
              i128::from(record.timestamp_ms) * 1_000_000,
            )
            .ok()
            .and_then(|time| time.format(&Rfc3339).ok())
            .unwrap_or_else(|| record.timestamp_ms.to_string());
            [
              time,
              serde_json::to_value(record.event)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned(),
              serde_json::to_value(record.outcome)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned(),
              record
                .subject_id
                .as_deref()
                .map_or_else(|| "—".into(), |id| id[..12].into()),
              if record.outcome == Outcome::Started {
                "—".into()
              } else {
                format!("{} ms", record.elapsed_ms)
              },
              match (record.error_code.as_deref(), record.os_error) {
                (Some(code), Some(os)) => format!("{code} ({os})"),
                (Some(code), None) => code.into(),
                (None, Some(os)) => os.to_string(),
                (None, None) => "—".into(),
              },
            ]
          })
        )
      );
    }
    if let Some(warning) = history.warning {
      eprintln!("Warning: {warning}");
    }
  }
  Ok(())
}
