//! Keep key-file passphrases on a trusted local path, separate from SSH prompts.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ctl_ipc::SshTarget;
use tokio::process::Command;
use zeroize::Zeroizing;

use crate::identities::{self, IdentitySnapshot, LocalAgent, VerifiedIdentity};

mod agent_proxy;
mod config;
mod fallback;
mod labels;
mod lazy;
use agent_proxy::AgentProxy;
#[cfg(test)]
use config::parse_configuration;
use config::{PublicKeyAuthentication, expand_home, resolve_configurations};
pub(super) use labels::{connection_context, save_offer_message};

const MAX_IDENTITIES: usize = 32;

/// This state is created only after failing to reuse an authenticated master.
/// Dropping it terminates its agent and all proxy clients, including cancellation.
#[derive(Default)]
pub(super) struct PreparedIdentities {
  snapshots: Vec<Arc<IdentitySnapshot>>,
  agent: Option<LocalAgent>,
  proxy: Option<AgentProxy>,
  public_files: Vec<(String, PathBuf)>,
  fallback_files: Vec<String>,
  fallbacks: Arc<fallback::Fallbacks>,
}

pub(super) struct VerifiedPassphrase {
  snapshot: Arc<IdentitySnapshot>,
  verified: VerifiedIdentity,
  secret: Zeroizing<String>,
}

impl VerifiedPassphrase {
  pub(super) fn name(&self) -> String {
    format!("SSH identity passphrase: {}", self.snapshot.path)
  }

  pub(super) fn save(self) -> Result<(), identities::IdentityError> {
    identities::save_verified(&self.snapshot, &self.verified, &self.secret)
  }
}

impl PreparedIdentities {
  pub(super) async fn prepare(target: &SshTarget) -> Self {
    let Ok(configurations) = resolve_configurations(target).await else {
      return Self::default();
    };
    let configuration = configurations.destination;
    let overlay_disabled = configuration.agent_disabled
      || configurations
        .gateways
        .iter()
        .any(|gateway| gateway.inherits_agent && gateway.mutates_agent);
    let mut prepared = Self {
      fallback_files: configuration.identity_files,
      ..Self::default()
    };
    let mut destination_ids = HashSet::new();
    let mut reusable_ids = HashSet::new();
    let mut candidates: Vec<_> = configuration
      .paths
      .into_iter()
      .map(|path| {
        (
          path,
          true,
          !configuration.agent_disabled
            && configuration.publickey_authentication == PublicKeyAuthentication::Enabled,
        )
      })
      .collect();
    for gateway in configurations.gateways {
      let reusable = gateway.inherits_agent
        && !gateway.agent_disabled
        && gateway.publickey_authentication == PublicKeyAuthentication::Enabled;
      candidates.extend(
        gateway
          .paths
          .into_iter()
          .map(|path| (path, false, reusable)),
      );
    }
    for (path, destination, reusable) in candidates {
      if prepared.snapshots.len() >= MAX_IDENTITIES {
        break;
      }
      let Ok(Ok(snapshot)) =
        tokio::task::spawn_blocking(move || identities::inspect_path(&path)).await
      else {
        continue;
      };
      if destination {
        destination_ids.insert(snapshot.identity_id.clone());
      }
      if reusable {
        reusable_ids.insert(snapshot.identity_id.clone());
      }
      if !prepared
        .snapshots
        .iter()
        .any(|existing| existing.identity_id == snapshot.identity_id)
      {
        prepared.snapshots.push(Arc::new(snapshot));
      }
    }
    // Explicit agent policy and mutation preferences retain native OpenSSH
    // behavior. Snapshots remain available for locally verified save offers.
    if overlay_disabled || reusable_ids.is_empty() {
      return prepared;
    }
    let candidates = prepared.saved_candidates(&reusable_ids).await;
    if candidates.is_empty() {
      return prepared;
    }
    let hints: Vec<_> = candidates
      .iter()
      .filter(|candidate| destination_ids.contains(&candidate.snapshot.identity_id))
      .map(|candidate| {
        (
          Arc::clone(&candidate.snapshot),
          candidate.public_key.clone(),
        )
      })
      .collect();
    prepared.start_proxy(
      candidates,
      connection_context(target),
      configuration.agent.as_deref(),
    );
    for (snapshot, public_key) in hints {
      prepared.add_public_hint(
        &snapshot,
        &public_key,
        &configuration.resolved_identity_files,
      );
    }
    prepared
  }

  fn start_proxy(
    &mut self,
    candidates: Vec<lazy::Candidate>,
    context: String,
    existing: Option<&Path>,
  ) {
    let candidate_ids: Vec<_> = candidates
      .iter()
      .map(|candidate| candidate.snapshot.identity_id.clone())
      .collect();
    match AgentProxy::with_identities(
      lazy::LazyIdentities::new(candidates, context, Arc::clone(&self.fallbacks)),
      existing,
    ) {
      Ok(proxy) => self.proxy = Some(proxy),
      Err(_) => {
        for identity_id in candidate_ids {
          self
            .fallbacks
            .record(&identity_id, fallback::Reason::AgentUnavailable);
        }
      }
    }
  }

  async fn saved_candidates(&self, reusable_ids: &HashSet<String>) -> Vec<lazy::Candidate> {
    let mut candidates = Vec::new();
    let mut public_keys = HashSet::new();
    for snapshot in &self.snapshots {
      if !snapshot.encrypted || !reusable_ids.contains(&snapshot.identity_id) {
        continue;
      }
      let selected = Arc::clone(snapshot);
      // This exact metadata/presence check cannot display authentication UI or
      // read a secret. Unsaved/unknown keys keep OpenSSH's native file flow.
      let hint =
        tokio::task::spawn_blocking(move || identities::saved_public_key_hint_checked(&selected))
          .await
          .map_err(|_| fallback::Reason::WorkerFailed)
          .and_then(|result| result.map_err(fallback::Reason::Identity))
          .and_then(|hint| match hint {
            identities::SavedPublicKeyHint::Available(public_key) => Ok(public_key),
            identities::SavedPublicKeyHint::Missing => Err(fallback::Reason::NotSaved),
            identities::SavedPublicKeyHint::Unavailable => {
              Err(fallback::Reason::PublicHintUnavailable)
            }
          });
      let public_key = match hint {
        Ok(public_key) => public_key,
        Err(reason) => {
          self.fallbacks.record(&snapshot.identity_id, reason);
          continue;
        }
      };
      let Some(candidate) = lazy::Candidate::new(Arc::clone(snapshot), public_key) else {
        self.fallbacks.record(
          &snapshot.identity_id,
          fallback::Reason::PublicHintUnavailable,
        );
        continue;
      };
      if public_keys.insert(candidate.blob.clone()) {
        candidates.push(candidate);
      }
    }
    candidates
  }

  pub(super) fn fallback_warning(&self, message: &str) -> Option<String> {
    if !is_key_prompt(message) {
      return None;
    }
    let path = prompt_path(message)?;
    if let Some(snapshot) = self
      .snapshots
      .iter()
      .find(|snapshot| snapshot.encrypted && path_matches(&path, &snapshot.path))
    {
      return self.fallbacks.warning(&snapshot.identity_id);
    }
    let mut snapshots = self
      .snapshots
      .iter()
      .filter(|snapshot| snapshot.encrypted && snapshot.path.starts_with(&path));
    let snapshot = snapshots.next()?;
    // A truncated path must still uniquely identify one configured identity.
    if snapshots.next().is_some() {
      return None;
    }
    self.fallbacks.warning(&snapshot.identity_id)
  }

  fn add_public_hint(
    &mut self,
    snapshot: &IdentitySnapshot,
    public_key: &str,
    configured_files: &[(String, String)],
  ) {
    if snapshot.fingerprint.is_some() {
      return;
    }
    if let Some(proxy) = &self.proxy {
      let Ok(path) = proxy.write_public_key(public_key, self.public_files.len()) else {
        self.fallbacks.record(
          &snapshot.identity_id,
          fallback::Reason::PublicHintUnavailable,
        );
        return;
      };
      for (configured, resolved) in configured_files {
        if path_matches(resolved, &snapshot.path) {
          self.public_files.push((configured.clone(), path.clone()));
        }
      }
    }
  }

  pub(super) fn append_options(&self, command: &mut Command) {
    if let Some(proxy) = &self.proxy {
      command
        // ProxyCommand/ProxyJump startup precedes IdentityAgent environment
        // application in some OpenSSH versions. Set the inherited view too.
        .env("SSH_AUTH_SOCK", proxy.socket_path())
        .arg("-o")
        .arg(format!("IdentityAgent={}", proxy.socket_path().display()));
      // Supplying even one -i suppresses OpenSSH's implicit default key list.
      // Preserve every effective identity, including keys without saved secrets.
      if !self.public_files.is_empty() {
        for path in &self.fallback_files {
          command.arg("-i").arg(path);
          // Only opaque PEM identities need a public hint. Keep it adjacent to
          // its configured file, preserving the configured identity ordering.
          for (_, public) in self
            .public_files
            .iter()
            .filter(|(configured, _)| configured == path)
          {
            command.arg("-i").arg(public);
          }
        }
      }
    }
  }

  /// Prompt text only selects a candidate already supplied by this connection's
  /// configuration. No saved secret is read here. A local ssh-add must prove the
  /// user-supplied candidate unlocks the exact snapshot before it can be saved.
  pub(super) async fn verify_captured(
    &mut self,
    captured: &mut HashMap<String, Zeroizing<String>>,
  ) -> Vec<VerifiedPassphrase> {
    let prompts: Vec<_> = captured
      .keys()
      .filter(|prompt| is_key_prompt(prompt))
      .cloned()
      .collect();
    // Drain all key candidates before verification, including on timeout or
    // local-agent failure. None may fall through to generic password storage.
    let candidates: Vec<_> = prompts
      .into_iter()
      .filter_map(|prompt| captured.remove(&prompt).map(|secret| (prompt, secret)))
      .collect();
    let mut verified = Vec::new();
    let mut seen = HashSet::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    for (prompt, secret) in candidates {
      let snapshots: Vec<_> = self
        .snapshots
        .iter()
        .filter(|snapshot| snapshot.encrypted)
        .collect();
      if snapshots.is_empty() {
        continue;
      }
      if self.agent.is_none() {
        self.agent = tokio::time::timeout_at(deadline, LocalAgent::start())
          .await
          .ok()
          .and_then(Result::ok);
      }
      let Some(agent) = &mut self.agent else { break };
      let path = prompt_path(&prompt);
      let paths: Vec<_> = snapshots
        .iter()
        .map(|snapshot| snapshot.path.as_str())
        .collect();
      for index in candidate_order(path.as_deref(), &paths) {
        let snapshot = snapshots[index];
        if seen.contains(&snapshot.identity_id) {
          continue;
        }
        let result =
          tokio::time::timeout_at(deadline, agent.add_identity(snapshot, secret.clone())).await;
        match result {
          Ok(Ok(identity)) => {
            seen.insert(snapshot.identity_id.clone());
            verified.push(VerifiedPassphrase {
              snapshot: Arc::clone(snapshot),
              verified: identity,
              secret,
            });
            break;
          }
          Ok(Err(_)) => {}
          Err(_) => return verified,
        }
      }
    }
    verified
  }
}

fn candidate_order(prompt_path: Option<&str>, paths: &[&str]) -> Vec<usize> {
  if let Some(prompt) = prompt_path
    && let Some(index) = paths.iter().position(|path| path_matches(prompt, path))
  {
    return vec![index];
  }
  // OpenSSH truncates key paths in prompts. Prefer a matching prefix, then
  // bounded configured candidates. Only local unlock success proves identity.
  let mut candidates: Vec<_> = (0..paths.len().min(MAX_IDENTITIES)).collect();
  candidates
    .sort_by_key(|index| !prompt_path.is_some_and(|prefix| paths[*index].starts_with(prefix)));
  candidates
}

pub(super) fn is_key_prompt(message: &str) -> bool {
  message
    .to_ascii_lowercase()
    .starts_with("enter passphrase for key")
}

fn prompt_path(message: &str) -> Option<String> {
  let rest = message
    .trim_end()
    .strip_prefix("Enter passphrase for key '")?;
  let path = rest.strip_suffix("':")?;
  (!path.is_empty() && !path.chars().any(char::is_control)).then(|| path.to_owned())
}

fn path_matches(prompt: &str, snapshot: &str) -> bool {
  // Canonicalization never reads the key's contents. Snapshot verification and
  // save_verified recheck its bytes before any Keychain mutation.
  std::fs::canonicalize(expand_home(prompt).unwrap_or_else(|| PathBuf::from(prompt)))
    .is_ok_and(|path| path == Path::new(snapshot))
}

#[cfg(test)]
mod tests;
