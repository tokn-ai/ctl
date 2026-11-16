//! Shared command-line infrastructure for ctl and ctlup.

pub mod caller_auth;
pub mod component_info;
pub mod connection;
pub mod openssh;
pub mod ssh_broker;
pub mod table;
pub mod target;
pub mod vpn_profiles;
pub mod vpn_runtime;

use clap::ValueEnum;
use ctl_core::component_update::BundleChannel;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RemotePlatform {
  Unix,
  Windows,
}

impl From<RemotePlatform> for crate::RemotePlatform {
  fn from(platform: RemotePlatform) -> Self {
    match platform {
      RemotePlatform::Unix => Self::Unix,
      RemotePlatform::Windows => Self::Windows,
    }
  }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Channel {
  Nightly,
  Stable,
}

impl From<Channel> for BundleChannel {
  fn from(channel: Channel) -> Self {
    match channel {
      Channel::Nightly => Self::Nightly,
      Channel::Stable => Self::Stable,
    }
  }
}
