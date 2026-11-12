use ctl_client::hosts::ConnectionTargetDto;
use ctl_client::{ConnectionTarget, SshConnectionOptions, SshInteraction};
use std::io;

pub(super) struct Target {
  pub destination: String,
  pub options: SshConnectionOptions,
  pub interaction: SshInteraction,
  host: ConnectionTargetDto,
}

impl Target {
  pub async fn connect(host: ConnectionTargetDto) -> io::Result<Self> {
    let ConnectionTarget::Ssh {
      destination,
      options,
    } = host.to_core()
    else {
      return Err(io::Error::other("the selected host is local; omit --host"));
    };
    crate::target::ensure_vpn(&host)
      .await
      .map_err(io::Error::other)?;
    #[cfg(unix)]
    let interaction = if options.remote_platform == ctl_client::RemotePlatform::Unix {
      let ssh = host.to_ssh_target().map_err(io::Error::other)?;
      let control_path = crate::ssh_broker::ensure_master(ssh)
        .await
        .map_err(io::Error::other)?;
      SshInteraction::Multiplexed { control_path }
    } else {
      SshInteraction::Inherit
    };
    #[cfg(windows)]
    let interaction = SshInteraction::Inherit;
    Ok(Self {
      destination,
      options,
      interaction,
      host,
    })
  }

  pub fn pinned(&self) -> bool {
    matches!(
      self.host,
      ConnectionTargetDto::Ssh {
        remote_info: Some(_),
        ..
      }
    )
  }

  pub fn expected_account(&self) -> Option<&str> {
    if matches!(self.interaction, SshInteraction::Multiplexed { .. }) {
      return None;
    }
    match &self.host {
      ConnectionTargetDto::Ssh {
        remote_info: Some(identity),
        ..
      } => Some(&identity.remote_id),
      _ => None,
    }
  }

  pub async fn verify_account(&self) -> io::Result<()> {
    if self.pinned() {
      let id = ctl_client::installation::remote_account_id(
        &self.destination,
        &self.options,
        &self.interaction,
      )
      .await
      .map_err(io::Error::other)?;
      self.host.verify_remote_id(&id).map_err(io::Error::other)?;
    }
    Ok(())
  }

  pub async fn verify_activation(&self, bundle: &ctl_core::bundles::Bundle) -> io::Result<()> {
    let active = if let SshInteraction::Multiplexed { control_path } = &self.interaction {
      let identity =
        ctl_client::maintenance::inspect_agent(&self.destination, &self.options, control_path)
          .await
          .map_err(io::Error::other)?;
      self
        .host
        .verify_remote_identity(&identity)
        .map_err(io::Error::other)?;
      let manifest = &bundle.manifest;
      let build = &manifest.components["ctl-agent"].build;
      identity.bundle.as_ref().is_some_and(|installed| {
        installed.bundle_id
          == *manifest
            .distribution_id
            .as_ref()
            .unwrap_or(&manifest.bundle_id)
          && installed.app_version == build.version
          && installed.target_triple == manifest.target_triple
          && Some(installed.git_revision.as_str()) == build.source_revision.as_deref()
      })
    } else {
      let status = ctl_client::installation::remote_status(
        &self.destination,
        &self.options,
        &self.interaction,
        self.expected_account(),
      )
      .await
      .map_err(io::Error::other)?;
      status.active.as_ref() == Some(&bundle.manifest)
    };
    if !active {
      return Err(io::Error::other(
        "Components installed, but activation could not be verified. Refresh status before retrying.",
      ));
    }
    Ok(())
  }

  pub async fn target_triple(&self) -> io::Result<&'static str> {
    let platform = tokio::time::timeout(std::time::Duration::from_mins(1), async {
      match self.options.remote_platform {
        ctl_client::RemotePlatform::Unix => {
          let output = ctl_client::probe_ssh_unix_platform_interactive(
            &self.destination,
            &self.options,
            &self.interaction,
          )
          .await
          .map_err(io::Error::other)?;
          ctl_client::remote_bundle::Platform::parse_probe(&output).map_err(io::Error::other)
        }
        ctl_client::RemotePlatform::Windows => ctl_client::probe_ssh_windows_platform(
          &self.destination,
          &self.options,
          &self.interaction,
        )
        .await
        .map_err(io::Error::other),
      }
    })
    .await
    .map_err(io::Error::other)??;
    platform.target_triple().map_err(io::Error::other)
  }
}
