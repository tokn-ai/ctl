use super::arguments::SignArguments;
use serde::{Deserialize, Serialize};
use std::{io, path::Path};

#[cfg(target_os = "macos")]
mod helper;
#[cfg(target_os = "macos")]
mod input;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod provisioning;
#[cfg(target_os = "macos")]
use macos::Runner;
#[cfg(target_os = "macos")]
pub(super) use macos::{
  DesktopSigningOptions, checkout_profile, export_desktop, select_provisioning,
  sign_ad_hoc_executable, sign_checkout_helper, sign_for_install, sign_runtime_helper,
};
#[cfg(target_os = "macos")]
pub(super) use provisioning::{has_profile_override, preferred_team, verify_embedded_profile};

/// Local macOS signing policy. The default is required for distribution.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SigningMode {
  #[default]
  DeveloperId,
  Provision,
  AdHoc,
}

/// A locally available desktop archive with a verified build identity.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
pub(super) struct DesktopInput<'a> {
  pub archive: &'a Path,
  pub app_version: &'a str,
  pub bundle_id: &'a str,
  pub source_revision: &'a str,
  pub installation_layout_version: Option<u16>,
  pub target: ctl_client::remote_bundle::desktop::DesktopTarget,
}

#[cfg(target_os = "macos")]
pub(super) async fn run(home: &Path, options: SignArguments) -> io::Result<()> {
  macos::run(home, options).await
}

#[cfg(not(target_os = "macos"))]
pub(super) fn run(_: &Path, _: SignArguments) -> std::future::Ready<io::Result<()>> {
  std::future::ready(Err(io::Error::new(
    io::ErrorKind::Unsupported,
    "Signing and notarizing macOS builds requires a Mac.",
  )))
}
