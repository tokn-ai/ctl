use ctl_ipc::credentials::{
  CredentialKind, Discovery, PasswordSource, PasswordState, SavedPassword,
};
use ctl_ipc::identities::FileState;

pub(crate) fn password(account_id: &str, name: &str) -> SavedPassword {
  SavedPassword {
    id: format!("{}:{account_id}", "a".repeat(64)),
    source: PasswordSource::Credential,
    name: name.into(),
    kind: CredentialKind::SshPassword,
    state: PasswordState::Saved,
    target: Some("alice@example.test".into()),
    account: Some("alice".into()),
    key_name: None,
    path: None,
    display_path: None,
    file_version: None,
    key_type: None,
    fingerprint: None,
    encrypted: None,
    file_state: None,
    detail: None,
    created_at_ms: Some(1),
    updated_at_ms: Some(2),
  }
}

pub(crate) fn identity(id: &str, state: PasswordState) -> SavedPassword {
  SavedPassword {
    id: id.into(),
    source: PasswordSource::Identity,
    name: "~/.ssh/id_ed25519".into(),
    kind: CredentialKind::SshKeyPassphrase,
    state,
    target: None,
    account: None,
    key_name: None,
    path: Some("/home/alice/.ssh/id_ed25519".into()),
    display_path: Some("~/.ssh/id_ed25519".into()),
    file_version: Some("version".into()),
    key_type: Some("ssh-ed25519".into()),
    fingerprint: Some("SHA256:public-fingerprint".into()),
    encrypted: Some(true),
    file_state: Some(FileState::Ready),
    detail: None,
    created_at_ms: None,
    updated_at_ms: None,
  }
}

pub(crate) fn discovery(entries: Vec<SavedPassword>) -> Discovery {
  Discovery {
    entries,
    complete: true,
    warnings: Vec::new(),
  }
}
