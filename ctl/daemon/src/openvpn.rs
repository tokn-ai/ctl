//! Converts a saved `OpenVPN` profile into the managed TUN container's input.

use std::io;

use ctl_ipc::{VpnConnection, VpnSettings, openvpn_profile};

use crate::{openconnect, vpn_container::RuntimeMetadata};

pub(super) fn prepare(connection: &VpnConnection) -> io::Result<openconnect::Config> {
  let VpnSettings::Openvpn {
    config,
    username,
    password,
  } = &connection.settings
  else {
    return Err(invalid("expected an OpenVPN profile"));
  };
  let content = openvpn_profile::render(
    config,
    username.as_deref(),
    password.as_ref().map(|value| value.as_str()),
  )
  .map_err(invalid)?;
  Ok(openconnect::Config {
    content,
    metadata: openconnect::Metadata::default(),
    runtime: RuntimeMetadata::for_connection(connection)?,
    cancellation: None,
  })
}

fn invalid(message: impl Into<String>) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
