//! Bounded binary echo for testing transport duplex, EOF, and cancellation.

use serde_json::json;
use std::{
  io::{self, Read, Write},
  thread,
  time::Duration,
};

const LIFETIME: Duration = Duration::from_secs(30);

pub fn echo() -> io::Result<()> {
  // A blocked stdin read or stdout write must not outlive a failed transport cancellation.
  thread::Builder::new()
    .name("stdio-timeout".into())
    .spawn(|| {
      thread::sleep(LIFETIME);
      std::process::exit(124);
    })?;
  let pid = std::process::id();
  writeln!(
    io::stderr().lock(),
    "{}",
    json!({"event": "ready", "pid": pid, "lifetime_ms": LIFETIME.as_millis()}),
  )?;
  let mut input = io::stdin().lock();
  let mut output = io::stdout().lock();
  let mut buffer = [0_u8; 16 * 1024];
  let mut bytes = 0_u64;
  loop {
    let count = match input.read(&mut buffer) {
      Ok(count) => count,
      Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
      Err(error) => return Err(error),
    };
    if count == 0 {
      writeln!(
        io::stderr().lock(),
        "{}",
        json!({"event": "eof", "pid": pid, "bytes": bytes}),
      )?;
      return Ok(());
    }
    output.write_all(&buffer[..count])?;
    output.flush()?;
    bytes += count as u64;
  }
}
