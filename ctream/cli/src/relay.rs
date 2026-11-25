use ctream_client::transport::{Channel, Transport};
use ctream_proto::{Target, agent_id_version};
use std::{io, sync::mpsc, thread};

/// A named session executes on the agent that owns the display and journal.
/// Forward stdin independently so either the peer's exit or local EOF can end it.
pub(super) fn named<T: Transport>(
  transport: T,
  target: Target,
  agent_id: &str,
  window: Option<String>,
) -> io::Result<()> {
  let version = agent_id_version(agent_id)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid agent ID"))?;
  let mut args = vec![
    version.into(),
    "attributed-session".into(),
    target.to_string(),
    agent_id.to_owned(),
  ];
  args.extend(window);
  let Channel {
    mut reader,
    mut writer,
    cancellation,
  } = transport.open(&args)?;
  let (finished, input_result) = mpsc::channel();
  thread::Builder::new()
    .name("ctream-session-stdin".into())
    .spawn(move || {
      let result = io::copy(&mut io::stdin().lock(), &mut writer);
      // Publish the result before closing the pipe that lets the host exit.
      let _ = finished.send(result);
      drop(writer);
    })?;
  let result = io::copy(&mut reader, &mut io::stdout().lock());
  cancellation.cancel();
  result?;
  input_result.try_recv().map_err(|_| {
    io::Error::new(
      io::ErrorKind::UnexpectedEof,
      "host session ended before input closed",
    )
  })??;
  Ok(())
}
