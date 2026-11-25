//! Run the high-level action on the display host, inside its journal boundary.
use ctream_agent::activity::Journal;
use ctream_client::{Client, sdk, transport::Local};
use ctream_proto::Target;
use std::io;

pub(super) fn run(
  target: Target,
  agent_id: &str,
  window: Option<&str>,
) -> ctream_driver::Result<()> {
  let mut journal = Journal::open()?;
  let local = Local {
    agent: std::env::current_exe()?,
  };
  #[cfg(target_os = "macos")]
  let local = LeasedDesktop {
    local,
    lease: uuid::Uuid::new_v4().simple().to_string(),
  };
  sdk::run_attributed_session(
    &Client::new(local),
    target,
    window.map(str::to_owned),
    io::stdin().lock(),
    io::stdout().lock(),
    |operation, apply| journal.record(agent_id, target, window, operation, apply),
  )?;
  Ok(())
}

// On macOS the GUI broker binds native input authorization to the lifetime
// of this session's own capture, including when another agent selects the same window.
#[cfg(target_os = "macos")]
struct LeasedDesktop {
  local: Local,
  lease: String,
}

#[cfg(target_os = "macos")]
impl ctream_client::transport::Transport for LeasedDesktop {
  type Reader = <Local as ctream_client::transport::Transport>::Reader;
  type Writer = <Local as ctream_client::transport::Transport>::Writer;

  fn open(
    &self,
    args: &[String],
  ) -> io::Result<ctream_client::transport::Channel<Self::Reader, Self::Writer>> {
    if let [version, operation, target, window] = args
      && target == "macos"
    {
      let owned = match (version.as_str(), operation.as_str()) {
        (ctream_proto::MACOS_WINDOW_VERSION, "capture-window") => Some("session-capture"),
        (ctream_proto::INPUT_SYNC_VERSION, "input-sync") => Some("session-input"),
        // An older desktop broker cannot prove whose capture authorizes input.
        (_, "input-window") => {
          return Err(io::Error::other(
            "named input requires an updated ctream-agent serve",
          ));
        }
        _ => None,
      };
      if let Some(operation) = owned {
        return self.local.open(&[
          ctream_proto::ATTRIBUTED_SESSION_VERSION.into(),
          operation.into(),
          target.clone(),
          window.clone(),
          self.lease.clone(),
        ]);
      }
    }
    self.local.open(args)
  }
}
