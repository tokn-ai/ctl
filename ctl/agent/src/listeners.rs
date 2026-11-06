use ctl_proto::{PortProtocol, TcpListenerCatalog};

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum Protocol {
  #[default]
  Tcp,
  Udp,
  Any,
}

impl Protocol {
  fn includes(self, protocol: PortProtocol) -> bool {
    matches!(self, Self::Any)
      || matches!(
        (self, protocol),
        (Self::Tcp, PortProtocol::Tcp) | (Self::Udp, PortProtocol::Udp)
      )
  }
}

/// Discovers TCP endpoints while preserving the original listener command.
#[must_use]
pub fn discover() -> TcpListenerCatalog {
  discover_for(Protocol::Tcp)
}

/// Discovers bound endpoints without reading process arguments or environment.
#[must_use]
pub fn discover_for(protocol: Protocol) -> TcpListenerCatalog {
  #[cfg(target_os = "linux")]
  return linux::discover(protocol);
  #[cfg(target_os = "macos")]
  return macos::discover(protocol);
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  {
    let _ = protocol;
    TcpListenerCatalog {
      listeners: Vec::new(),
      warnings: vec!["Listener discovery is not supported on this platform.".into()],
    }
  }
}

#[cfg(any(target_os = "linux", test))]
mod linux {
  use std::collections::BTreeSet;
  #[cfg(target_os = "linux")]
  use std::fs;
  use std::net::{Ipv4Addr, Ipv6Addr};

  #[cfg(target_os = "linux")]
  use super::Protocol;
  #[cfg(target_os = "linux")]
  use ctl_proto::TcpListenerCatalog;
  use ctl_proto::{PortProtocol, TcpListener};

  #[cfg(target_os = "linux")]
  pub fn discover(protocol: Protocol) -> TcpListenerCatalog {
    let mut listeners = BTreeSet::new();
    let mut warnings = Vec::new();
    for (path, ipv6, transport) in [
      ("/proc/net/tcp", false, PortProtocol::Tcp),
      ("/proc/net/tcp6", true, PortProtocol::Tcp),
      ("/proc/net/udp", false, PortProtocol::Udp),
      ("/proc/net/udp6", true, PortProtocol::Udp),
    ] {
      if !protocol.includes(transport) {
        continue;
      }
      match fs::read_to_string(path) {
        Ok(contents) => listeners.extend(parse_proc(&contents, ipv6, transport)),
        Err(error) => warnings.push(format!("Could not inspect {path}: {error}")),
      }
    }
    TcpListenerCatalog {
      listeners: listeners.into_iter().collect(),
      warnings,
    }
  }

  fn parse_proc(contents: &str, ipv6: bool, protocol: PortProtocol) -> Vec<TcpListener> {
    contents
      .lines()
      .skip(1)
      .filter_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        // Unconnected UDP sockets have state 07; TCP listeners have state 0A.
        let state = match protocol {
          PortProtocol::Tcp => "0A",
          PortProtocol::Udp => "07",
        };
        if fields.get(3) != Some(&state) {
          return None;
        }
        let (address, port) = fields.get(1)?.rsplit_once(':')?;
        let port = u16::from_str_radix(port, 16)
          .ok()
          .filter(|port| *port != 0)?;
        let bind_address = if ipv6 {
          parse_ipv6(address)?.to_string()
        } else {
          Ipv4Addr::from(u32::from_str_radix(address, 16).ok()?.to_le_bytes()).to_string()
        };
        Some(TcpListener {
          bind_address,
          port,
          protocol,
        })
      })
      .collect()
  }

  fn parse_ipv6(value: &str) -> Option<Ipv6Addr> {
    if value.len() != 32 {
      return None;
    }
    let mut bytes = [0_u8; 16];
    let (chunks, remainder) = value.as_bytes().as_chunks::<8>();
    if !remainder.is_empty() {
      return None;
    }
    for (index, chunk) in chunks.iter().enumerate() {
      let word = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
      bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    Some(Ipv6Addr::from(bytes))
  }

  #[cfg(test)]
  mod tests {
    use super::*;

    #[test]
    fn parses_only_listening_ipv4_and_ipv6_sockets() {
      let ipv4 = "  sl  local_address rem_address   st\n   0: 0100007F:1538 00000000:0000 0A\n   1: 0100007F:0016 00000000:0000 01\n";
      let ipv6 = "  sl  local_address rem_address   st\n   0: 00000000000000000000000001000000:0BB8 00000000000000000000000000000000:0000 0A\n";
      assert_eq!(
        parse_proc(ipv4, false, PortProtocol::Tcp),
        vec![TcpListener {
          bind_address: "127.0.0.1".into(),
          port: 5432,
          protocol: PortProtocol::Tcp,
        }]
      );
      assert_eq!(
        parse_proc(ipv6, true, PortProtocol::Tcp),
        vec![TcpListener {
          bind_address: "::1".into(),
          port: 3000,
          protocol: PortProtocol::Tcp,
        }]
      );
    }

    #[test]
    fn parses_only_unconnected_udp_and_retains_protocol_identity() {
      let rows = "  sl local_address rem_address st\n 0: 00000000:0035 00000000:0000 07\n 1: 0100007F:1538 0100007F:1234 01\n 2: 00000000:0000 00000000:0000 07\n";
      assert_eq!(
        parse_proc(rows, false, PortProtocol::Udp),
        vec![TcpListener {
          bind_address: "0.0.0.0".into(),
          port: 53,
          protocol: PortProtocol::Udp,
        }]
      );
      let rows = "  sl local_address rem_address st\n 0: 00000000000000000000000001000000:0035 00000000000000000000000000000000:0000 07\n";
      assert_eq!(
        parse_proc(rows, true, PortProtocol::Udp)[0].bind_address,
        "::1"
      );
      let mut endpoints = BTreeSet::new();
      for protocol in [PortProtocol::Tcp, PortProtocol::Udp] {
        endpoints.insert(TcpListener {
          bind_address: "127.0.0.1".into(),
          port: 53,
          protocol,
        });
      }
      assert_eq!(endpoints.len(), 2);
    }
  }
}

#[cfg(any(target_os = "macos", test))]
mod macos {
  use std::collections::BTreeSet;
  #[cfg(target_os = "macos")]
  use std::process::Command;

  #[cfg(target_os = "macos")]
  use super::Protocol;
  #[cfg(target_os = "macos")]
  use ctl_proto::TcpListenerCatalog;
  use ctl_proto::{PortProtocol, TcpListener};

  #[cfg(target_os = "macos")]
  pub fn discover(protocol: Protocol) -> TcpListenerCatalog {
    let mut listeners = BTreeSet::new();
    let mut warnings = Vec::new();
    for transport in [PortProtocol::Tcp, PortProtocol::Udp] {
      if !protocol.includes(transport) {
        continue;
      }
      let mut command = Command::new("/usr/sbin/lsof");
      command.args(["-nP", "-Fn"]);
      let label = match transport {
        PortProtocol::Tcp => {
          command.args(["-iTCP", "-sTCP:LISTEN"]);
          "TCP"
        }
        PortProtocol::Udp => {
          command.arg("-iUDP");
          "UDP"
        }
      };
      let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
          warnings.push(format!("Could not inspect {label} listeners: {error}"));
          continue;
        }
      };
      let stdout = String::from_utf8_lossy(&output.stdout);
      listeners.extend(
        stdout
          .lines()
          .filter_map(|line| parse_name_field(line, transport)),
      );
      // lsof returns 1 with no stderr when no matching sockets exist.
      if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if !message.is_empty() {
          warnings.push(message);
        }
      }
    }
    TcpListenerCatalog {
      listeners: listeners.into_iter().collect(),
      warnings,
    }
  }

  fn parse_name_field(line: &str, protocol: PortProtocol) -> Option<TcpListener> {
    let endpoint = line.strip_prefix('n')?;
    if endpoint.contains("->") {
      return None;
    }
    let (address, port) = endpoint.rsplit_once(':')?;
    let port = port.parse().ok().filter(|port| *port != 0)?;
    let bind_address = address
      .strip_prefix('[')
      .and_then(|value| value.strip_suffix(']'))
      .unwrap_or(address);
    Some(TcpListener {
      bind_address: match bind_address {
        "*" => "0.0.0.0".into(),
        value => value.to_owned(),
      },
      port,
      protocol,
    })
  }

  #[cfg(test)]
  mod tests {
    use super::*;

    #[test]
    fn parses_lsof_name_fields_without_connections() {
      for protocol in [PortProtocol::Tcp, PortProtocol::Udp] {
        assert_eq!(
          parse_name_field("n127.0.0.1:5432", protocol),
          Some(TcpListener {
            bind_address: "127.0.0.1".into(),
            port: 5432,
            protocol,
          })
        );
        assert_eq!(
          parse_name_field("n127.0.0.1:5432->127.0.0.1:60000", protocol),
          None
        );
        assert_eq!(
          parse_name_field("n[::1]:53", protocol)
            .unwrap()
            .bind_address,
          "::1"
        );
        assert_eq!(
          parse_name_field("n*:53", protocol).unwrap().bind_address,
          "0.0.0.0"
        );
      }
    }
  }
}
