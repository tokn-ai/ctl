//! A viewer's observation channel owns its identity; input attaches privately over stdin.
use ctream_agent::protocol::ViewerLane;
use ctream_agent::{
  daemon::{Authority, SourceObserver},
  native_input::{self, Mode},
};
use ctream_client::{live::ViewerAttachment, sdk::SessionProtocol};
use ctream_proto::Target;
use std::io::{self, Read, Write};

pub(super) fn run(
  target: Target,
  window: Option<&str>,
  lane: ViewerLane,
) -> ctream_driver::Result<()> {
  let lease = match lane {
    ViewerLane::Observe(label) => {
      let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
      return Ok(runtime.block_on(observe(target, window, label))?);
    }
    ViewerLane::Control { lease } => lease,
    ViewerLane::Semantic => None,
  };
  let mut input = io::BufReader::new(io::stdin().lock());
  let attachment = ViewerAttachment::read(&mut input)?;
  let authority = Authority::attach(target, window, attachment.as_secret())?;
  if matches!(lane, ViewerLane::Semantic) {
    return crate::attributed::serve_with(
      authority,
      target,
      window,
      SessionProtocol::Daemon,
      input,
    );
  }
  Ok(native_input::serve_controller(
    Mode::Viewer { receipts: true },
    authority,
    None,
    lease,
    input,
    &mut io::stdout().lock(),
  )?)
}

async fn observe(target: Target, window: Option<&str>, label: &str) -> io::Result<()> {
  let source = [(target, window.map(str::to_owned))];
  let mut observer = SourceObserver::connect(&std::env::current_exe()?, label, &source).await?;
  let (left, mut closed) = tokio::sync::oneshot::channel();
  std::thread::Builder::new()
    .name("ctream-viewer-observer".into())
    .spawn(move || {
      let mut byte = [0u8];
      let result = match io::stdin().lock().read(&mut byte) {
        Ok(0) => Ok(()),
        Ok(_) => Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "viewer observation is read-only",
        )),
        Err(error) => Err(error),
      };
      let _ = left.send(result);
    })?;
  let mut out = io::stdout().lock();
  writeln!(out, "{}", observer.attachment())?;
  out.flush()?;
  loop {
    tokio::select! {
      result = &mut closed => return result.map_err(io::Error::other)?,
      next = observer.next_event() => {
        let Some(item) = next? else {return Ok(())};
        serde_json::to_writer(&mut out,&item.event)?;
        out.write_all(b"\n")?;
        out.flush()?;
      }
    }
  }
}
