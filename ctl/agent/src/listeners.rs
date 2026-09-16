use std::io;

use ctl_proto::TcpListenerCatalog;

/// Discovers listening TCP endpoints without reading process arguments or environment.
///
/// # Errors
/// Returns an error only when the platform discovery process cannot be started.
pub fn discover() -> io::Result<TcpListenerCatalog> {
  platform::discover()
}

#[cfg(target_os = "linux")]
mod platform {
  use std::collections::BTreeSet;
  use std::fs;
  use std::io;
  use std::net::{Ipv4Addr, Ipv6Addr};

  use ctl_proto::{TcpListener, TcpListenerCatalog};

  pub fn discover() -> io::Result<TcpListenerCatalog> {
    let mut listeners = BTreeSet::new();
    let mut warnings = Vec::new();
    for (path, ipv6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
      match fs::read_to_string(path) {
        Ok(contents) => listeners.extend(parse_proc(&contents, ipv6)),
        Err(error) => warnings.push(format!("Could not inspect {path}: {error}")),
      }
    }
    Ok(TcpListenerCatalog {
      listeners: listeners.into_iter().collect(),
      warnings,
    })
  }

  fn parse_proc(contents: &str, ipv6: bool) -> Vec<TcpListener> {
    contents
      .lines()
      .skip(1)
      .filter_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.get(3) != Some(&"0A") {
          return None;
        }
        let (address, port) = fields.get(1)?.rsplit_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        let bind_address = if ipv6 {
          parse_ipv6(address)?.to_string()
        } else {
          Ipv4Addr::from(u32::from_str_radix(address, 16).ok()?.to_le_bytes()).to_string()
        };
        Some(TcpListener { bind_address, port })
      })
      .collect()
  }

  fn parse_ipv6(value: &str) -> Option<Ipv6Addr> {
    if value.len() != 32 {
      return None;
    }
    let mut bytes = [0_u8; 16];
    for (index, chunk) in value.as_bytes().chunks_exact(8).enumerate() {
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
        parse_proc(ipv4, false),
        vec![TcpListener {
          bind_address: "127.0.0.1".into(),
          port: 5432,
        }]
      );
      assert_eq!(
        parse_proc(ipv6, true),
        vec![TcpListener {
          bind_address: "::1".into(),
          port: 3000,
        }]
      );
    }
  }
}

#[cfg(target_os = "macos")]
mod platform {
  use std::collections::BTreeSet;
  use std::io;
  use std::process::Command;

  use ctl_proto::{TcpListener, TcpListenerCatalog};

  pub fn discover() -> io::Result<TcpListenerCatalog> {
    let output = Command::new("/usr/sbin/lsof")
      .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fn"])
      .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let listeners: BTreeSet<_> = stdout.lines().filter_map(parse_name_field).collect();
    let mut warnings = Vec::new();
    if !output.status.success() {
      let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
      warnings.push(if message.is_empty() {
        format!("lsof exited with {}", output.status)
      } else {
        message
      });
    }
    Ok(TcpListenerCatalog {
      listeners: listeners.into_iter().collect(),
      warnings,
    })
  }

  fn parse_name_field(line: &str) -> Option<TcpListener> {
    let endpoint = line.strip_prefix('n')?;
    if endpoint.contains("->") {
      return None;
    }
    let (address, port) = endpoint.rsplit_once(':')?;
    let port = port.parse().ok()?;
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
    })
  }

  #[cfg(test)]
  mod tests {
    use super::*;

    #[test]
    fn parses_lsof_name_fields_without_connections() {
      assert_eq!(
        parse_name_field("n127.0.0.1:5432"),
        Some(TcpListener {
          bind_address: "127.0.0.1".into(),
          port: 5432,
        })
      );
      assert_eq!(parse_name_field("n127.0.0.1:5432->127.0.0.1:60000"), None);
    }
  }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
  use std::io;

  use ctl_proto::TcpListenerCatalog;

  pub fn discover() -> io::Result<TcpListenerCatalog> {
    Ok(TcpListenerCatalog {
      listeners: Vec::new(),
      warnings: vec!["TCP listener discovery is not supported on this platform.".into()],
    })
  }
}
