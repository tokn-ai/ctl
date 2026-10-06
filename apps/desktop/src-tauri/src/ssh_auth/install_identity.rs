//! Component uploads must verify a saved account pin without opening a service.

use std::future::Future;
use std::path::Path;

use ctl_client::{CoreError, SshConnectionOptions};

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

pub(super) fn verify_installed(
  installed: &crate::dto::RemoteAgentInstallResultDto,
  identity: &ctl_proto::RemoteIdentity,
  agent_only: Option<&ctl_core::component::ComponentInfo>,
) -> CommandResult<()> {
  if let Some(agent) = agent_only {
    if ctl_client::component_update::agent_matches(agent, identity) {
      return Ok(());
    }
  } else if identity.bundle.as_ref().is_some_and(|bundle| {
    bundle.bundle_id == installed.bundle_id
      && bundle.git_revision == installed.git_revision
      && bundle.target_triple == installed.target_triple
      && bundle.app_version == installed.app_version
  }) {
    return Ok(());
  }
  Err(CommandErrorDto::new(
    "remote_install_verification_failed",
    "Components were installed, but the active installation differs from the verified bundle. Running sessions were preserved. Refresh Components before retrying.",
  ))
}

pub(super) async fn upload<T, F, U>(
  target: &ConnectionTargetDto,
  destination: &str,
  options: &SshConnectionOptions,
  control_path: &Path,
  operation: F,
) -> CommandResult<T>
where
  F: FnOnce() -> U,
  U: Future<Output = CommandResult<T>>,
{
  verify_then_upload(
    target,
    || ctl_client::maintenance::inspect_agent(destination, options, control_path),
    operation,
  )
  .await
}

async fn verify_then_upload<T, I, F, U>(
  target: &ConnectionTargetDto,
  inspect: impl FnOnce() -> I,
  upload: F,
) -> CommandResult<T>
where
  I: Future<Output = Result<ctl_proto::RemoteIdentity, CoreError>>,
  F: FnOnce() -> U,
  U: Future<Output = CommandResult<T>>,
{
  if matches!(
    target,
    ConnectionTargetDto::Ssh {
      remote_info: Some(_),
      ..
    }
  ) {
    let identity = inspect().await.map_err(|error| {
      CommandErrorDto::new(
        "remote_update_identity_verification_failed",
        format!(
          "Could not verify this SSH host's saved remote identity before updating its components. No components were uploaded. Reconnect and verify the host before updating it: {error}"
        ),
      )
    })?;
    target.verify_remote_identity(&identity)?;
  }
  upload().await
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::cell::Cell;
  use std::future::ready;

  #[test]
  fn activation_requires_the_exact_selected_bundle_not_the_app_revision() {
    let installed = crate::dto::RemoteAgentInstallResultDto {
      app_version: "0.1.0".into(),
      bundle_id: "0.1.0-dev.cached".into(),
      git_revision: "a".repeat(40),
      target_triple: "x86_64-unknown-linux-musl".into(),
    };
    let mut observed = identity("11111111-1111-4111-8111-111111111111");
    assert!(verify_installed(&installed, &observed, None).is_err());
    observed.bundle = Some(Box::new(ctl_proto::BundleVersion {
      app_version: installed.app_version.clone(),
      bundle_id: installed.bundle_id.clone(),
      git_revision: installed.git_revision.clone(),
      target_triple: installed.target_triple.clone(),
    }));
    assert!(verify_installed(&installed, &observed, None).is_ok());
    observed.bundle.as_mut().unwrap().bundle_id = "a-different-installation".into();
    assert!(verify_installed(&installed, &observed, None).is_err());
  }

  #[test]
  fn agent_only_verifies_compiled_metadata_without_requiring_a_new_source_receipt() {
    let expected = ctl_core::component::ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: ctl_proto::agent_protocols(),
    };
    let installed = crate::dto::RemoteAgentInstallResultDto {
      app_version: expected.build.version.clone(),
      bundle_id: "source-bundle".into(),
      git_revision: expected.build.source_revision.clone().unwrap_or_default(),
      target_triple: ctl_core::paths::native_target().into(),
    };
    let mut observed = identity("11111111-1111-4111-8111-111111111111");
    observed.agent_version.clone_from(&expected.build.version);
    observed.build = Some(expected.build.clone());
    observed.protocols.clone_from(&expected.protocols);
    assert!(observed.bundle.is_none());
    assert!(verify_installed(&installed, &observed, Some(&expected)).is_ok());
    observed.build.as_mut().unwrap().source_fingerprint = "f".repeat(64);
    assert!(verify_installed(&installed, &observed, Some(&expected)).is_err());
    observed.build = Some(expected.build.clone());
    observed.protocols.pop();
    assert!(verify_installed(&installed, &observed, Some(&expected)).is_err());
  }

  fn identity(remote_id: &str) -> ctl_proto::RemoteIdentity {
    serde_json::from_value(serde_json::json!({
      "remote_id": remote_id, "agent_version": "0.1.0",
    }))
    .unwrap()
  }

  fn pinned_target() -> ConnectionTargetDto {
    serde_json::from_value(serde_json::json!({
      "kind": "ssh", "destination": "jump-a",
      "remote_info": identity("11111111-1111-4111-8111-111111111111"),
    }))
    .unwrap()
  }

  #[tokio::test]
  async fn matching_passive_identity_is_verified_before_any_upload() {
    let inspected = Cell::new(false);
    let uploaded = Cell::new(false);
    let result = verify_then_upload(
      &pinned_target(),
      || {
        inspected.set(true);
        ready(Ok(identity("11111111-1111-4111-8111-111111111111")))
      },
      || {
        assert!(inspected.get());
        uploaded.set(true);
        ready(Ok("installed"))
      },
    )
    .await
    .unwrap();
    assert_eq!(result, "installed");
    assert!(uploaded.get());
  }

  #[tokio::test]
  async fn changed_remote_identity_blocks_upload_with_the_existing_mismatch_code() {
    let error = verify_then_upload(
      &pinned_target(),
      || ready(Ok(identity("22222222-2222-4222-8222-222222222222"))),
      || -> std::future::Ready<CommandResult<()>> {
        panic!("a mismatched SSH account must never receive component bytes")
      },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "remote_identity_mismatch");
  }

  #[tokio::test]
  async fn unsupported_or_failed_passive_inspection_blocks_upload_without_a_service_fallback() {
    for inspection in [
      CoreError::SshCommandFailed {
        status: "exit status: 2".into(),
        diagnostic: "error: unrecognized subcommand 'inspect'".into(),
      },
      CoreError::InvalidSshCommandOutput,
      CoreError::SshCommandFailed {
        status: "exit status: 255".into(),
        diagnostic: "Permission denied (publickey).".into(),
      },
    ] {
      let error = verify_then_upload(
        &pinned_target(),
        || ready(Err(inspection)),
        || -> std::future::Ready<CommandResult<()>> {
          panic!("unverified SSH accounts must never receive component bytes")
        },
      )
      .await
      .unwrap_err();
      assert_eq!(error.code, "remote_update_identity_verification_failed");
      assert!(error.message.contains("No components were uploaded"));
    }
  }

  #[tokio::test]
  async fn new_unpinned_host_installations_do_not_require_an_existing_agent() {
    let installed = verify_then_upload(
      &ConnectionTargetDto::ssh("new-host"),
      || -> std::future::Ready<Result<ctl_proto::RemoteIdentity, CoreError>> {
        panic!("an unpinned new host must not need an installed agent")
      },
      || ready(Ok("installed")),
    )
    .await
    .unwrap();
    assert_eq!(installed, "installed");
  }
}
