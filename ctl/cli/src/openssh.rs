//! Compatibility entry points. OpenSSH retains ownership of command and copy
//! semantics; ctl only supplies saved-host settings and a compatible master.

use std::ffi::{OsStr, OsString};
use std::process::Command;

use crate::connection::{self, Error};

pub const SCP_TRANSPORT_ENV: &str = "CTL_SCP_SSH_TRANSPORT";

#[derive(Debug)]
struct Invocation {
  destination: Option<usize>,
  reuse: bool,
  inspect: bool,
  route_override: bool,
}

/// Find the destination without interpreting remote command arguments. Keep
/// unknown options for the installed SSH version to diagnose itself.
fn inspect(arguments: &[OsString]) -> Invocation {
  let mut result = Invocation {
    destination: None,
    reuse: true,
    inspect: false,
    route_override: false,
  };
  let mut index = 0;
  while index < arguments.len() {
    let Some(argument) = arguments[index].to_str() else {
      result.destination = Some(index);
      break;
    };
    if argument == "--" {
      result.destination = (index + 1 < arguments.len()).then_some(index + 1);
      break;
    }
    if !argument.starts_with('-') || argument == "-" {
      result.destination = Some(index);
      break;
    }
    for (offset, flag) in argument[1..].char_indices() {
      if "GQV".contains(flag) {
        result.inspect = true;
        result.reuse = false;
      }
      if "BbcDEeFIiJLlmOoPpQRSWw".contains(flag) {
        let tail = &argument[offset + 2..];
        let value = if tail.is_empty() {
          index += 1;
          arguments
            .get(index)
            .and_then(|value| value.to_str())
            .unwrap_or("")
        } else {
          tail
        };
        if flag == 'o' {
          let (name, _) = value.split_once(['=', ' ']).unwrap_or((value, ""));
          if ["proxyjump", "proxycommand"].contains(&name.to_ascii_lowercase().as_str()) {
            result.route_override = true;
          }
          // scp supplies these session settings to its SSH subprocess.
          if ![
            "clearallforwardings",
            "remotecommand",
            "requesttty",
            "forwardagent",
            "permitlocalcommand",
            "loglevel",
            "sendenv",
            "setenv",
          ]
          .contains(&name.to_ascii_lowercase().as_str())
          {
            result.reuse = false;
          }
        } else if !"eELDRW".contains(flag) {
          result.reuse = false;
        }
        if flag == 'O' {
          result.reuse = false;
        }
        if flag == 'J' {
          result.route_override = true;
        }
        break;
      }
      if !"vqtTnsx".contains(flag) {
        result.reuse = false;
      }
    }
    index += 1;
  }
  result
}

pub async fn run_ssh(mut arguments: Vec<OsString>, method: Option<&str>) -> Result<i32, Error> {
  let invocation = inspect(&arguments);
  let mut command = Command::new("ssh");
  command.env_remove(SCP_TRANSPORT_ENV);
  let Some(index) = invocation.destination else {
    command.args(arguments);
    return connection::run_process(command);
  };
  let destination = arguments[index]
    .to_str()
    .ok_or_else(|| Error::Arguments("SSH destinations must be UTF-8.".into()))?;
  let resolved = crate::target::resolve(Some(destination), method).await?;
  let managed = resolved.host_id.is_some();
  // Raw destinations retain OpenSSH's full configuration behavior. Saved hosts
  // opt into ctl defaults, and safe session options can share ctld's master.
  let mut target = resolved.target.to_ssh_target()?;
  if invocation.route_override {
    target.gateways.clear();
  }
  let mut defaults = if managed {
    connection::target_arguments(&target)?
  } else {
    Vec::new()
  };
  if managed && !invocation.reuse {
    defaults.extend([OsString::from("-o"), OsString::from("ControlPath=none")]);
  }
  if managed && !invocation.inspect {
    if !invocation.route_override {
      crate::target::ensure_vpn(&resolved.target).await?;
    }
    #[cfg(unix)]
    if invocation.reuse {
      let socket = crate::ssh_broker::ensure_master(target.clone()).await?;
      let mut master = vec![
        OsString::from("-S"),
        socket.into_os_string(),
        "-o".into(),
        "ControlMaster=no".into(),
        "-o".into(),
        "ProxyCommand=false".into(),
      ];
      master.append(&mut defaults);
      defaults = master;
    }
  }
  if managed {
    arguments[index] = target.destination.into();
  }
  // Explicit OpenSSH options precede ctl defaults. A -- option terminator must
  // remain after all injected options.
  let insert = if index > 0 && arguments[index - 1] == OsStr::new("--") {
    index - 1
  } else {
    index
  };
  arguments.splice(insert..insert, defaults);
  command.args(arguments);
  connection::run_process(command)
}

pub fn run_scp(arguments: Vec<OsString>, method: Option<&str>) -> Result<i32, Error> {
  let mut command = Command::new("scp");
  command
    .arg("-S")
    .arg(std::env::current_exe()?)
    .args(arguments);
  command.env(SCP_TRANSPORT_ENV, "1");
  if let Some(method) = method {
    command.env("CTL_SCP_METHOD", method);
  }
  connection::run_process(command)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
  }

  #[test]
  fn only_options_before_the_destination_affect_connection_reuse() {
    let parsed = inspect(&args(&["-vvtt", "work", "echo", "-i", "remote-file"]));
    assert_eq!(parsed.destination, Some(1));
    assert!(parsed.reuse);
    for values in [
      &["-p2222", "work"][..],
      &["-i", "key", "work"],
      &["-oProxyCommand=custom", "work"],
      &["-S", "socket", "work"],
    ] {
      assert!(!inspect(&args(values)).reuse);
    }
  }

  #[test]
  fn scp_transport_options_and_subsystem_reuse_the_master() {
    let parsed = inspect(&args(&[
      "-x",
      "-oPermitLocalCommand=no",
      "-oClearAllForwardings=yes",
      "-oRemoteCommand=none",
      "-oRequestTTY=no",
      "-s",
      "--",
      "work",
      "sftp",
    ]));
    assert!(parsed.reuse);
    assert_eq!(parsed.destination, Some(7));
  }

  #[test]
  fn inspection_never_opens_a_connection() {
    for values in [&["-G", "work"][..], &["-V"], &["-Q", "cipher"]] {
      let parsed = inspect(&args(values));
      assert!(parsed.inspect);
      assert!(!parsed.reuse);
    }
  }
}
