//! Host-side SDK adapter. The daemon owns all input and its audit lifecycle.
use ctream_agent::{
  daemon::Authority,
  desktop_input::{DesktopTransport, InputRequest},
};
use ctream_client::{
  Client,
  sdk::{self, Mutation, SessionAuthority, SessionProtocol},
};
use ctream_proto::Target;
use std::io;

pub(super) fn serve(
  target: Target,
  agent_id: &str,
  window: Option<&str>,
  protocol: SessionProtocol,
) -> ctream_driver::Result<()> {
  let session_id = uuid::Uuid::new_v4().to_string();
  let authority = Authority::connect(target, window, agent_id, &session_id)?;
  serve_with(authority, target, window, protocol, io::stdin().lock())
}

pub(super) fn serve_with(
  authority: Authority,
  target: Target,
  window: Option<&str>,
  protocol: SessionProtocol,
  input: impl io::Read,
) -> ctream_driver::Result<()> {
  let client = Client::new(DesktopTransport::new(None)?);
  let mut controller = Controller(authority);
  let session = sdk::run_host_session(
    &client,
    target,
    window.map(str::to_owned),
    input,
    io::stdout().lock(),
    protocol,
    &mut controller,
  );
  session.and(controller.0.close())?;
  Ok(())
}

struct Controller(Authority);

impl SessionAuthority for Controller {
  fn acquire(&mut self, ttl_ms: u64) -> io::Result<String> {
    self.0.acquire(ttl_ms)
  }
  fn release(&mut self, token: &str) -> io::Result<()> {
    self.0.release(token)
  }
  fn dispatch(&mut self, token: Option<&str>, action: Mutation) -> io::Result<String> {
    self.0.execute(token, InputRequest::Sdk { action })
  }
}
