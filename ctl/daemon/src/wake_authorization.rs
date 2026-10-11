//! One explicit native authorization batch for live shells after full wake.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use ctl_ipc::{GatewayKind, SshTarget};
use ctl_keychain_client::AuthenticationContext;

use crate::target_lifecycle::TargetAttempt;
use crate::{State, approval_context::Shared, keychain::approval, keychain::approval_scope};

#[cfg(test)]
mod tests;

pub(super) fn spawn(state: Arc<State>) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let Some(approvals) = &state.approvals else {
      return;
    };
    let (sender, mut resumes) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = approvals.session().subscribe_resume(Arc::new(move |event| {
      let _ = sender.send(event);
    }));
    while let Some(event) = resumes.recv().await {
      authorize(&state, event.generation).await;
    }
  })
}

struct Scope {
  target: SshTarget,
  name: String,
  prefix: String,
  selectors: Vec<String>,
  members: Vec<(SshTarget, TargetAttempt)>,
}

struct BatchGuard {
  batch: Arc<Shared<AuthenticationContext>>,
  committed: bool,
  monitor: tokio::task::JoinHandle<()>,
}

impl Drop for BatchGuard {
  fn drop(&mut self) {
    self.monitor.abort();
    if !self.committed {
      self.batch.native.invalidate();
    }
  }
}

fn current(state: &State, generation: u64) -> bool {
  state.approvals.as_ref().is_some_and(|approvals| {
    approvals
      .session()
      .resume_is_current(ctl_keychain_client::SessionResume { generation })
  })
}

fn route_targets(target: &SshTarget) -> Vec<SshTarget> {
  let mut targets = vec![target.clone()];
  targets.extend(
    target
      .gateways
      .iter()
      .enumerate()
      .filter_map(|(index, gateway)| {
        (gateway.kind == GatewayKind::Vpn)
          .then(|| ctl_ipc::vpn_owner_target(&target.gateways, index))
          .flatten()
      }),
  );
  targets
}

async fn prepare(state: &State, hosts: &[SshTarget]) -> (Vec<SshTarget>, Vec<Scope>) {
  let mut scopes: HashMap<String, Scope> = HashMap::new();
  let mut eligible = Vec::new();
  for host in hosts {
    if state.target(host).is_paused() {
      continue;
    }
    let mut route = Vec::new();
    for target in route_targets(host) {
      let Some(scope) = prepare_scope(state, target).await else {
        route.clear();
        break;
      };
      route.push(scope);
    }
    // Hosts without saved protected credentials need no new native consent.
    // Their ordinary quiet connection policy continues independently.
    if !route.iter().any(|scope| !scope.selectors.is_empty()) {
      continue;
    }
    eligible.push(host.clone());
    for mut scope in route {
      if let Some(existing) = scopes.get_mut(&scope.name) {
        for member in scope.members.drain(..) {
          if !existing
            .members
            .iter()
            .any(|(target, _)| target == &member.0)
          {
            existing.members.push(member);
          }
        }
      } else {
        scopes.insert(scope.name.clone(), scope);
      }
    }
  }
  let mut scopes: Vec<_> = scopes.into_values().collect();
  scopes.sort_by(|a, b| a.name.cmp(&b.name));
  for scope in &mut scopes {
    scope.selectors.sort();
  }
  (eligible, scopes)
}

async fn prepare_scope(state: &State, target: SshTarget) -> Option<Scope> {
  let lifecycle = state.target(&target);
  let attempt = lifecycle.attempt();
  if lifecycle.is_paused() {
    return None;
  }
  let approvals = state.approvals.as_ref()?;
  let prefix = approval_scope::prefix(&target);
  let fingerprint = approval_scope::snapshot(&target).await?;
  let name = format!("{prefix}{fingerprint}");
  let mut selectors = approvals.selectors(&name);
  if selectors.is_empty() {
    selectors = approval::selectors::discover(&target).await.ok()?;
  }
  Some(Scope {
    members: vec![(target.clone(), attempt)],
    target,
    name,
    prefix,
    selectors,
  })
}

async fn authorize(state: &Arc<State>, generation: u64) {
  if !current(state, generation) {
    return;
  }
  let hosts = state.wake_recovery.active_before(generation);
  if hosts.is_empty() {
    return;
  }
  let (hosts, scopes) = prepare(state, &hosts).await;
  if hosts.is_empty() || !current(state, generation) {
    return;
  }
  let Some(approvals) = &state.approvals else {
    return;
  };
  let creating = Arc::clone(approvals);
  let Ok(Ok(batch)) = tokio::task::spawn_blocking(move || creating.wake_batch()).await else {
    return;
  };
  let monitor = monitor(
    Arc::clone(state),
    Arc::clone(&batch),
    hosts.clone(),
    generation,
  );
  let mut guard = BatchGuard {
    batch: Arc::clone(&batch),
    committed: false,
    monitor,
  };
  let mut completed = std::collections::HashSet::new();
  for scope in &scopes {
    if !current(state, generation) {
      return;
    }
    if !scope
      .members
      .iter()
      .any(|(target, _)| has_live_host(state, &hosts, target, generation))
    {
      continue;
    }
    if scope.selectors.is_empty() {
      completed.insert(scope.name.clone());
    } else {
      match authorize_scope(state, scope, &hosts, &batch, generation).await {
        Ok(true) => {
          completed.insert(scope.name.clone());
        }
        Ok(false) => {}
        Err(()) => return,
      }
    }
  }
  // Revalidate configuration/trust after consent before publishing any success.
  for scope in &scopes {
    if !completed.contains(&scope.name) {
      continue;
    }
    let Some(fingerprint) = approval_scope::snapshot(&scope.target).await else {
      return;
    };
    if format!("{}{fingerprint}", scope.prefix) != scope.name {
      return;
    }
  }
  approvals.maintain();
  if !current(state, generation) || batch.native.is_invalidated() {
    return;
  }
  let scopes: Vec<_> = scopes
    .into_iter()
    .filter(|scope| completed.contains(&scope.name))
    .collect();
  publish(state, &hosts, &scopes, generation);
  guard.committed = true;
}

fn publish(state: &State, hosts: &[SshTarget], scopes: &[Scope], generation: u64) {
  let mut authorized = std::collections::HashSet::new();
  for scope in scopes {
    for (target, attempt) in &scope.members {
      if has_live_host(state, hosts, target, generation) && state.target(target).authorized(attempt)
      {
        authorized.insert(crate::target_key(target));
      }
    }
  }
  for host in hosts {
    if state.wake_recovery.contains(host, generation)
      && !state.target(host).is_paused()
      && route_targets(host)
        .iter()
        .all(|target| authorized.contains(&crate::target_key(target)))
    {
      state.wake_recovery.authorized(host, generation);
    }
  }
}

fn has_live_host(state: &State, hosts: &[SshTarget], target: &SshTarget, generation: u64) -> bool {
  hosts.iter().any(|host| {
    state.wake_recovery.contains(host, generation)
      && !state.target(host).is_paused()
      && route_targets(host).iter().any(|route| route == target)
  })
}

async fn authorize_scope(
  state: &Arc<State>,
  scope: &Scope,
  hosts: &[SshTarget],
  batch: &Arc<Shared<AuthenticationContext>>,
  generation: u64,
) -> Result<bool, ()> {
  let approvals = state.approvals.clone();
  let batch = Arc::clone(batch);
  let prefix = scope.prefix.clone();
  let name = scope.name.clone();
  let members = scope.members.clone();
  let hosts = hosts.to_vec();
  let selectors = scope.selectors.clone();
  let reason = wake_reason(&hosts);
  let state = Arc::clone(state);
  tokio::task::spawn_blocking(move || {
    if !current(&state, generation) {
      return Err(());
    }
    let live = |target: &SshTarget, attempt: &TargetAttempt| {
      has_live_host(&state, &hosts, target, generation)
        && matches!(
          attempt.status(),
          crate::target_lifecycle::AttemptStatus::Current
        )
    };
    let Some((target, _)) = members
      .iter()
      .find(|(target, attempt)| live(target, attempt))
    else {
      return Ok(false);
    };
    let native = Arc::clone(&batch.native);
    let history = ctl_core::observability::ConnectionContext {
      subject: crate::target_key(target),
      endpoint: crate::history_endpoint(target),
      correlation_id: state.correlation(
        &state
          .existing_endpoint(target)
          .ok()
          .flatten()
          .unwrap_or_else(|| crate::MasterEndpoint::managed(target)),
      ),
    };
    let Ok(authorization) =
      approval::Attempt::begin(approvals.as_ref(), &prefix, Some(name), true, history)
        .for_wake(batch)
    else {
      return Err(());
    };
    let _guard = authorization.guard();
    for selector in selectors {
      if !current(&state, generation) {
        return Err(());
      }
      if !members
        .iter()
        .any(|(target, attempt)| live(target, attempt))
      {
        return Ok(false);
      }
      if authorization.reauthorize(&selector, &reason).is_err() {
        return if !native.is_invalidated()
          && !members
            .iter()
            .any(|(target, attempt)| live(target, attempt))
        {
          Ok(false)
        } else {
          Err(())
        };
      }
    }
    if !current(&state, generation) {
      return Err(());
    }
    if !members
      .iter()
      .any(|(target, attempt)| live(target, attempt))
    {
      return Ok(false);
    }
    if authorization.connected() {
      Ok(true)
    } else {
      Err(())
    }
  })
  .await
  .unwrap_or(Err(()))
}

fn wake_reason(hosts: &[SshTarget]) -> String {
  let mut names: Vec<_> = hosts.iter().map(|host| host.destination.as_str()).collect();
  names.sort_unstable();
  names.dedup();
  let shown: Vec<String> = names
    .iter()
    .take(6)
    .map(|name| name.chars().take(120).collect())
    .collect();
  let mut hosts = shown.join(", ");
  if names.len() > shown.len() {
    write!(hosts, " and {} more hosts", names.len() - shown.len()).expect("writing to a String");
  }
  format!("Resume open SSH shells for {hosts} after sleep")
}

fn monitor(
  state: Arc<State>,
  batch: Arc<Shared<AuthenticationContext>>,
  hosts: Vec<SshTarget>,
  generation: u64,
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
      interval.tick().await;
      if !current(&state, generation)
        || !hosts.iter().any(|host| {
          state.wake_recovery.contains(host, generation) && !state.target(host).is_paused()
        })
      {
        batch.native.invalidate();
        return;
      }
    }
  })
}
