//! Cooperative lifecycle control on the owner-only ctld endpoint.

use ctld_ipc::lifecycle::{DaemonBinaryInfo, DaemonInfo, Request, Response};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

use super::{RequestError, State};

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum FirstMessage {
  Lifecycle(Request),
  Client(Box<ctld_ipc::ClientMessage>),
}

pub(super) struct Control {
  pub(super) instance_id: String,
  restart: mpsc::Sender<ctld_ipc::Stream>,
}

impl Control {
  pub(super) fn new() -> (Self, mpsc::Receiver<ctld_ipc::Stream>) {
    let (restart, receiver) = mpsc::channel(1);
    (
      Self {
        instance_id: uuid::Uuid::new_v4().to_string(),
        restart,
      },
      receiver,
    )
  }
}

pub(super) async fn handle(
  mut stream: ctld_ipc::Stream,
  state: &State,
  request: Request,
) -> Result<(), RequestError> {
  let Some(control) = &state.lifecycle else {
    return Err(RequestError::InvalidRequest(
      "lifecycle control is unavailable",
    ));
  };
  if !matches!(request, Request::CtldInspect { protocol_version } if protocol_version == ctld_ipc::lifecycle::PROTOCOL_VERSION)
  {
    return error(
      &mut stream,
      "ctld_lifecycle_incompatible",
      "A compatible lifecycle inspection must precede restart.",
    )
    .await;
  }
  let active_vpn_count = if let Some(service) = &state.vpn_service {
    service
      .list()
      .await
      .map_err(RequestError::VpnFailed)?
      .connections
      .into_iter()
      .filter(|connection| {
        connection.state != ctld_ipc::VpnState::Stopped
          && connection.locally_connected != Some(false)
      })
      .count()
  } else {
    0
  };
  let info = DaemonInfo {
    instance_id: control.instance_id.clone(),
    binary: DaemonBinaryInfo::current(),
    active_vpn_count: u32::try_from(active_vpn_count).unwrap_or(u32::MAX),
  };
  ctld_ipc::write_frame(&mut stream, &Response::CtldInfo { info }).await?;
  // Native confirmation tokens expire after one minute; retain this exact
  // process connection long enough to confirm without reselecting an owner.
  let Ok(request) = timeout(
    Duration::from_secs(90),
    ctld_ipc::read_frame::<_, Request>(&mut stream),
  )
  .await
  else {
    return Ok(());
  };
  let Some(request) = request? else {
    return Ok(());
  };
  if !matches!(request, Request::CtldRestart { expected_instance_id } if expected_instance_id == control.instance_id)
  {
    return error(
      &mut stream,
      "ctld_owner_changed",
      "The selected ctld owner changed. Check versions again.",
    )
    .await;
  }
  // Transfer the stream to the main owner loop. It acknowledges the request,
  // drains VPNs and forwards, releases its socket, and only then sends EOF.
  if let Err(failed) = control.restart.try_send(stream) {
    let mut stream = failed.into_inner();
    return error(
      &mut stream,
      "ctld_restart_in_progress",
      "ctld is already restarting.",
    )
    .await;
  }
  Ok(())
}

async fn error(
  stream: &mut ctld_ipc::Stream,
  code: &str,
  message: &str,
) -> Result<(), RequestError> {
  ctld_ipc::write_frame(
    stream,
    &Response::CtldError {
      code: code.into(),
      message: message.into(),
    },
  )
  .await
  .map_err(Into::into)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Arc;

  #[tokio::test]
  async fn passive_inspection_reports_instance_without_a_data_handshake() {
    let (control, mut restarts) = Control::new();
    let expected = control.instance_id.clone();
    let state = Arc::new(State {
      lifecycle: Some(control),
      ..State::default()
    });
    let (mut client, server) = ctld_ipc::Stream::pair().unwrap();
    let handler = tokio::spawn(super::super::handle_connection(server, state));
    ctld_ipc::write_frame(
      &mut client,
      &Request::CtldInspect {
        protocol_version: ctld_ipc::lifecycle::PROTOCOL_VERSION,
      },
    )
    .await
    .unwrap();
    let Some(Response::CtldInfo { info }) = ctld_ipc::read_frame(&mut client).await.unwrap() else {
      panic!("missing identity");
    };
    assert_eq!(info.instance_id, expected);
    assert_eq!(info.binary, DaemonBinaryInfo::current());
    assert_eq!(info.active_vpn_count, 0);
    drop(client);
    handler.await.unwrap().unwrap();
    assert!(restarts.try_recv().is_err());
  }

  #[tokio::test]
  async fn wrong_instance_cannot_submit_restart() {
    let (control, mut restarts) = Control::new();
    let state = Arc::new(State {
      lifecycle: Some(control),
      ..State::default()
    });
    let (mut client, server) = ctld_ipc::Stream::pair().unwrap();
    let handler = tokio::spawn(super::super::handle_connection(server, state));
    ctld_ipc::write_frame(
      &mut client,
      &Request::CtldInspect {
        protocol_version: ctld_ipc::lifecycle::PROTOCOL_VERSION,
      },
    )
    .await
    .unwrap();
    ctld_ipc::read_frame::<_, Response>(&mut client)
      .await
      .unwrap();
    ctld_ipc::write_frame(
      &mut client,
      &Request::CtldRestart {
        expected_instance_id: "another-owner".into(),
      },
    )
    .await
    .unwrap();
    assert!(
      matches!(ctld_ipc::read_frame::<_, Response>(&mut client).await.unwrap(), Some(Response::CtldError { code, .. }) if code == "ctld_owner_changed")
    );
    handler.await.unwrap().unwrap();
    assert!(restarts.try_recv().is_err());
  }

  #[tokio::test]
  async fn accepted_restart_transfers_stream_to_owner_until_cleanup_completes() {
    let (control, mut restarts) = Control::new();
    let expected = control.instance_id.clone();
    let state = Arc::new(State {
      lifecycle: Some(control),
      ..State::default()
    });
    let (mut client, server) = ctld_ipc::Stream::pair().unwrap();
    let handler = tokio::spawn(super::super::handle_connection(server, state));
    ctld_ipc::write_frame(
      &mut client,
      &Request::CtldInspect {
        protocol_version: ctld_ipc::lifecycle::PROTOCOL_VERSION,
      },
    )
    .await
    .unwrap();
    ctld_ipc::read_frame::<_, Response>(&mut client)
      .await
      .unwrap();
    ctld_ipc::write_frame(
      &mut client,
      &Request::CtldRestart {
        expected_instance_id: expected,
      },
    )
    .await
    .unwrap();
    let owned = restarts.recv().await.unwrap();
    handler.await.unwrap().unwrap();
    assert!(
      timeout(
        Duration::from_millis(20),
        ctld_ipc::read_frame::<_, Response>(&mut client)
      )
      .await
      .is_err()
    );
    drop(owned);
    assert!(
      ctld_ipc::read_frame::<_, Response>(&mut client)
        .await
        .unwrap()
        .is_none()
    );
  }
}
