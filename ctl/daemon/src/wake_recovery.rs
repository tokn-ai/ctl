//! Recovery intent belongs to a live frontend socket, never historical masters.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use ctl_ipc::{ClientMessage, ServerMessage, SshTarget};
use tokio::sync::watch;

use crate::{RequestError, State};

const MAX_HOSTS: usize = 128;
const MAX_LEASES: usize = 256;

struct Registration {
  #[cfg_attr(
    not(any(target_os = "macos", test)),
    expect(dead_code, reason = "only macOS wake monitoring filters sleep epochs")
  )]
  generation: u64,
  authorized: watch::Sender<Option<u64>>,
}

#[derive(Default)]
struct Registrations {
  next: u64,
  event_revision: u64,
  count: usize,
  hosts: HashMap<SshTarget, HashMap<u64, Registration>>,
}

#[derive(Default)]
pub(super) struct Registry(Mutex<Registrations>);

/// Dropping the dedicated watch socket removes its exact host intent.
pub(super) struct Lease {
  registry: Weak<Registry>,
  target: SshTarget,
  id: u64,
}

impl Registry {
  pub(super) fn active(&self) -> Vec<SshTarget> {
    self.0.lock().unwrap().hosts.keys().cloned().collect()
  }

  pub(super) fn is_live(&self, target: &SshTarget) -> bool {
    self.0.lock().unwrap().hosts.contains_key(target)
  }

  pub(super) fn register(
    self: &Arc<Self>,
    target: SshTarget,
    generation: u64,
  ) -> Result<(Lease, watch::Receiver<Option<u64>>), &'static str> {
    let mut registrations = self.0.lock().unwrap();
    if registrations.count >= MAX_LEASES
      || (!registrations.hosts.contains_key(&target) && registrations.hosts.len() >= MAX_HOSTS)
    {
      return Err("too many active recovery watches");
    }
    let id = registrations.next;
    registrations.next = id.checked_add(1).ok_or("recovery watch IDs exhausted")?;
    let (authorized, receiver) = watch::channel(None);
    registrations
      .hosts
      .entry(target.clone())
      .or_default()
      .insert(
        id,
        Registration {
          generation,
          authorized,
        },
      );
    registrations.count += 1;
    Ok((
      Lease {
        registry: Arc::downgrade(self),
        target,
        id,
      },
      receiver,
    ))
  }

  #[cfg(any(target_os = "macos", test))]
  pub(super) fn active_before(&self, generation: u64) -> Vec<SshTarget> {
    self
      .0
      .lock()
      .unwrap()
      .hosts
      .iter()
      .filter(|(_, watches)| watches.values().any(|watch| watch.generation < generation))
      .map(|(target, _)| target.clone())
      .collect()
  }

  #[cfg(any(target_os = "macos", test))]
  pub(super) fn contains(&self, target: &SshTarget, generation: u64) -> bool {
    self
      .0
      .lock()
      .unwrap()
      .hosts
      .get(target)
      .is_some_and(|watches| watches.values().any(|watch| watch.generation < generation))
  }

  #[cfg(any(target_os = "macos", test))]
  pub(super) fn authorized(&self, target: &SshTarget, generation: u64) {
    self.notify(target, |watch| watch.generation < generation);
  }

  /// An interactive success permits currently open shells to retry quietly.
  /// Its event revision is independent of the session epoch used for wake.
  pub(super) fn connected(&self, target: &SshTarget) {
    self.notify(target, |_| true);
  }

  fn notify(&self, target: &SshTarget, eligible: impl Fn(&Registration) -> bool) {
    let mut registrations = self.0.lock().unwrap();
    if !registrations
      .hosts
      .get(target)
      .is_some_and(|watches| watches.values().any(&eligible))
    {
      return;
    }
    let Some(revision) = registrations.event_revision.checked_add(1) else {
      // Never repeat a revision and let clients mistake a new grant for an old one.
      return;
    };
    registrations.event_revision = revision;
    if let Some(watches) = registrations.hosts.get(target) {
      for registration in watches.values().filter(|watch| eligible(watch)) {
        registration.authorized.send_replace(Some(revision));
      }
    }
  }
}

/// Credential principals retain the exact endpoint, account, identity, and route;
/// OpenSSH master ownership is an independent connection policy.
#[cfg(any(target_os = "macos", test))]
pub(super) fn principal(target: &SshTarget) -> SshTarget {
  let mut target = target.clone();
  target.use_ssh_config_master = None;
  if target.hostname.is_none() && target.ssh_config_alias.as_deref() == Some(&target.destination) {
    target.ssh_config_alias = None;
  }
  target
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn route_targets(target: &SshTarget) -> Vec<SshTarget> {
  let mut targets = vec![target.clone()];
  targets.extend(
    target
      .gateways
      .iter()
      .enumerate()
      .filter_map(|(index, gateway)| {
        (gateway.kind == ctl_ipc::GatewayKind::Vpn)
          .then(|| ctl_ipc::vpn_owner_target(&target.gateways, index))
          .flatten()
      }),
  );
  targets
}

impl Drop for Lease {
  fn drop(&mut self) {
    let Some(registry) = self.registry.upgrade() else {
      return;
    };
    let mut registrations = registry.0.lock().unwrap();
    let empty = if let Some(watches) = registrations.hosts.get_mut(&self.target) {
      let removed = watches.remove(&self.id).is_some();
      let empty = watches.is_empty();
      if removed {
        registrations.count -= 1;
      }
      empty
    } else {
      false
    };
    if empty {
      registrations.hosts.remove(&self.target);
    }
  }
}

pub(super) async fn watch(
  stream: &mut ctl_ipc::Stream,
  state: &State,
  target: SshTarget,
) -> Result<(), RequestError> {
  crate::validate_target(&target)?;
  #[cfg(target_os = "macos")]
  let generation = state
    .approvals
    .as_ref()
    .map_or(0, |approvals| approvals.session().generation());
  #[cfg(not(target_os = "macos"))]
  let generation = 0;
  let (_lease, mut authorized) = state
    .wake_recovery
    .register(target, generation)
    .map_err(RequestError::InvalidRequest)?;
  let (mut reader, mut writer) = tokio::io::split(stream);
  ctl_ipc::write_frame(&mut writer, &ServerMessage::RecoveryRegistered).await?;
  // Keep this read pinned across notifications: canceling a partially read
  // frame would discard bytes and corrupt the watch's request stream.
  let incoming = ctl_ipc::read_frame::<_, ClientMessage>(&mut reader);
  tokio::pin!(incoming);
  loop {
    tokio::select! {
      biased;
      message = &mut incoming => {
        return match message? {
          None => Ok(()),
          Some(_) => Err(RequestError::InvalidRequest("recovery watches accept no further requests")),
        };
      }
      changed = authorized.changed() => {
        changed.map_err(|_| RequestError::ClientClosed)?;
        let generation = *authorized.borrow_and_update();
        if let Some(generation) = generation {
          ctl_ipc::write_frame(&mut writer, &ServerMessage::RecoveryAuthorized { generation }).await?;
        }
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn target(name: &str) -> SshTarget {
    SshTarget {
      destination: name.into(),
      ssh_config_alias: None,
      use_ssh_config_master: Some(false),
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      gateways: Vec::new(),
    }
  }

  #[test]
  fn only_live_pre_sleep_intent_is_selected_and_notified() {
    let registry = Arc::new(Registry::default());
    let selected = target("selected");
    let (first, mut first_updates) = registry.register(selected.clone(), 0).unwrap();
    let (second, mut second_updates) = registry.register(selected.clone(), 0).unwrap();
    let (new, new_updates) = registry.register(selected.clone(), 1).unwrap();
    let (inactive, _) = registry.register(target("inactive"), 0).unwrap();
    drop(inactive);
    let (after_sleep, after_updates) = registry.register(target("after-sleep"), 1).unwrap();
    assert_eq!(registry.active_before(1), std::slice::from_ref(&selected));
    assert!(registry.contains(&selected, 1));
    registry.authorized(&selected, 1);
    assert_eq!(*first_updates.borrow_and_update(), Some(1));
    assert_eq!(*second_updates.borrow_and_update(), Some(1));
    assert_eq!(*new_updates.borrow(), None);
    assert_eq!(*after_updates.borrow(), None);
    drop(first);
    assert!(registry.contains(&selected, 1));
    drop(second);
    assert!(!registry.contains(&selected, 1));
    assert_eq!(registry.active_before(1), [] as [SshTarget; 0]);
    registry.authorized(&selected, 1);
    assert_eq!(*new_updates.borrow(), None);
    drop(new);
    drop(after_sleep);
    assert!(registry.0.lock().unwrap().hosts.is_empty());
  }

  #[test]
  fn manual_events_notify_same_epoch_watches_with_distinct_revisions_after_wake() {
    let registry = Arc::new(Registry::default());
    let selected = target("selected");
    let (_old, mut old) = registry.register(selected.clone(), 49).unwrap();
    let (_duplicate, mut duplicate) = registry.register(selected.clone(), 49).unwrap();
    let (_current, mut current) = registry.register(selected.clone(), 50).unwrap();
    registry.authorized(&selected, 50);
    assert_eq!(*old.borrow_and_update(), Some(1));
    assert_eq!(*duplicate.borrow_and_update(), Some(1));
    assert_eq!(*current.borrow(), None);
    registry.connected(&selected);
    for updates in [&mut old, &mut duplicate, &mut current] {
      assert_eq!(*updates.borrow_and_update(), Some(2));
    }
    registry.connected(&selected);
    for updates in [&mut old, &mut duplicate, &mut current] {
      assert_eq!(*updates.borrow_and_update(), Some(3));
    }
    // No event is persisted for inactive or newly registered historical hosts.
    registry.connected(&target("inactive"));
  }

  #[test]
  fn host_and_duplicate_limits_release_capacity_with_the_socket() {
    let registry = Arc::new(Registry::default());
    let mut leases = Vec::new();
    for index in 0..MAX_HOSTS {
      leases.push(
        registry
          .register(target(&format!("host-{index}")), 0)
          .unwrap()
          .0,
      );
    }
    assert!(registry.register(target("extra-host"), 0).is_err());
    leases.pop();
    leases.push(registry.register(target("extra-host"), 0).unwrap().0);
    let selected = target("host-0");
    for _ in MAX_HOSTS..MAX_LEASES {
      leases.push(registry.register(selected.clone(), 0).unwrap().0);
    }
    assert!(registry.register(selected.clone(), 0).is_err());
    leases.pop();
    assert!(registry.register(selected, 0).is_ok());
  }

  #[tokio::test]
  async fn watch_socket_lifetime_releases_its_host_and_delivers_authorization() {
    let state = Arc::new(State::default());
    let target = target("fixture.invalid");
    let mut expected = target.clone();
    expected.normalize_master_policy();
    let (mut client, server) = ctl_ipc::Stream::pair().unwrap();
    let worker_state = Arc::clone(&state);
    let worker = tokio::spawn(crate::handle_connection(server, worker_state));
    crate::handshake(&mut client).await.unwrap();
    ctl_ipc::write_frame(&mut client, &ClientMessage::WatchRecovery { target })
      .await
      .unwrap();
    assert!(matches!(
      ctl_ipc::read_frame::<_, ServerMessage>(&mut client)
        .await
        .unwrap(),
      Some(ServerMessage::RecoveryRegistered)
    ));
    assert!(state.wake_recovery.contains(&expected, 1));
    state.wake_recovery.authorized(&expected, 1);
    assert!(matches!(
      ctl_ipc::read_frame::<_, ServerMessage>(&mut client)
        .await
        .unwrap(),
      Some(ServerMessage::RecoveryAuthorized { generation: 1 })
    ));
    drop(client);
    tokio::time::timeout(std::time::Duration::from_secs(1), worker)
      .await
      .unwrap()
      .unwrap()
      .unwrap();
    assert!(!state.wake_recovery.contains(&expected, 1));
  }

  #[test]
  fn historical_contracts_reject_watches_before_registering() {
    let request = ClientMessage::WatchRecovery {
      target: target("fixture.invalid"),
    };
    for protocol in ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS.iter().copied() {
      let result = crate::validate_request_contract(&request, protocol);
      assert_eq!(result.is_ok(), ctl_ipc::recovery_watch_supported(protocol));
    }
  }
}
