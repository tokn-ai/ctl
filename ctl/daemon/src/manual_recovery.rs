//! A completed interactive connection lets existing shells retry quietly.

use ctl_ipc::SshTarget;

use crate::State;
use crate::target_lifecycle::{AttemptStatus, TargetAttempt};

#[derive(Clone)]
struct Candidate {
  host: SshTarget,
  host_attempt: TargetAttempt,
  route: SshTarget,
  route_attempt: TargetAttempt,
  exact: bool,
}

pub(super) struct Capture {
  #[cfg(target_os = "macos")]
  source: SshTarget,
  source_attempt: TargetAttempt,
  candidates: Vec<Candidate>,
}

pub(super) struct Ready {
  source_attempt: TargetAttempt,
  candidates: Vec<Candidate>,
  #[cfg(target_os = "macos")]
  approval: Option<ApprovalFence>,
}

#[cfg(target_os = "macos")]
struct ApprovalFence {
  approvals: std::sync::Arc<crate::keychain::approval::Approvals>,
  observed: crate::keychain::approval::Reusable,
}

impl Capture {
  /// Capture live intent and lifecycle fences before connection/config awaits.
  pub(super) fn new(state: &State, source: &SshTarget) -> Self {
    let source_attempt = state.target(source).attempt();
    let candidates = state
      .wake_recovery
      .active()
      .into_iter()
      .filter_map(|host| {
        let host_attempt = state.target(&host).attempt();
        if &host != source && !current(&host_attempt) {
          return None;
        }
        let route = matched_route(&host, source)?;
        let route_attempt = state.target(&route).attempt();
        if &route != source && !current(&route_attempt) {
          return None;
        }
        Some(Candidate {
          exact: &host == source,
          host,
          host_attempt,
          route,
          route_attempt,
        })
      })
      .collect();
    Self {
      #[cfg(target_os = "macos")]
      source: source.clone(),
      source_attempt,
      candidates,
    }
  }

  /// Preparation observes committed grants and current configuration only.
  /// It neither authenticates nor obtains any credential value.
  pub(super) async fn prepare(&self, state: &std::sync::Arc<State>) -> Ready {
    #[cfg(target_os = "macos")]
    {
      let (reusable, approval) = self.reusable_routes(state).await;
      let mut ready = self.prepared(|route| reusable.contains(route));
      ready.approval = approval;
      ready
    }
    #[cfg(not(target_os = "macos"))]
    {
      let _ = state;
      self.prepared(|_| false)
    }
  }

  fn prepared(&self, reusable: impl Fn(&SshTarget) -> bool) -> Ready {
    Ready {
      source_attempt: self.source_attempt.clone(),
      candidates: self
        .candidates
        .iter()
        .filter(|candidate| candidate.exact || reusable(&candidate.route))
        .cloned()
        .collect(),
      #[cfg(target_os = "macos")]
      approval: None,
    }
  }

  #[cfg(target_os = "macos")]
  async fn reusable_routes(
    &self,
    state: &std::sync::Arc<State>,
  ) -> (Vec<SshTarget>, Option<ApprovalFence>) {
    use crate::keychain::approval_scope;
    let routes = self.approval_routes();
    if routes.is_empty() {
      return (Vec::new(), None);
    }
    let Some(approvals) = &state.approvals else {
      return (Vec::new(), None);
    };
    let Some(fingerprint) = approval_scope::snapshot(&self.source).await else {
      return (Vec::new(), None);
    };
    let scope = format!("{}{fingerprint}", approval_scope::prefix(&self.source));
    let mut reusable = Vec::new();
    // Shared/private master policies never make their control paths equivalent.
    // They can only reuse the same independently checked credential principal.
    for route in routes {
      if approval_scope::snapshot(&route).await.as_ref() == Some(&fingerprint) {
        reusable.push(route);
      }
    }
    if reusable.is_empty() {
      return (Vec::new(), None);
    }
    // Observe the committed grant last, after all configuration/trust awaits.
    let approvals = std::sync::Arc::clone(approvals);
    let worker = std::sync::Arc::clone(&approvals);
    let Some(observed) = tokio::task::spawn_blocking(move || worker.reusable(&scope))
      .await
      .ok()
      .flatten()
    else {
      return (Vec::new(), None);
    };
    (
      reusable,
      Some(ApprovalFence {
        approvals,
        observed,
      }),
    )
  }

  #[cfg(any(target_os = "macos", test))]
  fn approval_routes(&self) -> Vec<SshTarget> {
    let mut seen = std::collections::HashSet::new();
    self
      .candidates
      .iter()
      .filter(|candidate| !candidate.exact)
      .map(|candidate| candidate.route.clone())
      .filter(|route| seen.insert(route.clone()))
      .collect()
  }
}

impl Ready {
  /// Publish synchronously at the committed ownership boundary, before replying.
  /// EOF after `MasterReady` must not cancel this notification.
  pub(super) fn publish(self, state: &State) {
    self.publish_using(state, || {
      #[cfg(target_os = "macos")]
      {
        self
          .approval
          .as_ref()
          .is_none_or(|approval| approval.approvals.still_reusable(&approval.observed))
      }
      #[cfg(not(target_os = "macos"))]
      true
    });
  }

  fn publish_using(&self, state: &State, current_approval: impl Fn() -> bool) {
    if !current(&self.source_attempt) {
      return;
    }
    for candidate in &self.candidates {
      if !candidate.exact && !current_approval() {
        continue;
      }
      if state.wake_recovery.is_live(&candidate.host)
        && current(&candidate.host_attempt)
        && current(&candidate.route_attempt)
        && state
          .target(&candidate.route)
          .authorized(&candidate.route_attempt)
        && state
          .target(&candidate.host)
          .authorized(&candidate.host_attempt)
      {
        state.wake_recovery.connected(&candidate.host);
      }
    }
  }
}

fn current(attempt: &TargetAttempt) -> bool {
  matches!(attempt.status(), AttemptStatus::Current)
}

fn matched_route(host: &SshTarget, source: &SshTarget) -> Option<SshTarget> {
  if host == source {
    return Some(host.clone());
  }
  #[cfg(any(target_os = "macos", test))]
  {
    let principal = crate::wake_recovery::principal(source);
    crate::wake_recovery::route_targets(host)
      .into_iter()
      .find(|route| crate::wake_recovery::principal(route) == principal)
  }
  #[cfg(not(any(target_os = "macos", test)))]
  None
}

#[cfg(test)]
mod tests;
