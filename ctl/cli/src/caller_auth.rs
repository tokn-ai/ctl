//! Caller-owned credentials never enter ctld, saved-host storage or audit metadata.
mod channel;
mod host_trust;
use crate::{Arguments, Command};
use ctl_client::{
  caller_auth::{Authentication, Credential},
  hosts::ConnectionTargetDto,
};
use std::io::Write as _;
use std::{io, path::PathBuf, sync::Arc};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

pub const ASKPASS_MODE: &str = "CTL_CALLER_ASKPASS";

pub fn claim(arguments: &mut Arguments) -> io::Result<()> {
  validate(arguments)?;
  if let Some(fd) = arguments.password_fd {
    arguments.password_input = Some(channel::claim(fd)?);
  }
  Ok(())
}

pub struct CallerAuth {
  pub authentication: Authentication,
  task: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(test)]
impl CallerAuth {
  pub(crate) fn for_test(authentication: Authentication) -> Self {
    Self {
      authentication,
      task: None,
    }
  }
}

impl Drop for CallerAuth {
  fn drop(&mut self) {
    if let Some(task) = &self.task {
      task.abort();
    }
  }
}

pub fn validate(arguments: &Arguments) -> io::Result<()> {
  if arguments.user.is_none()
    && arguments.port.is_none()
    && arguments.identity_file.is_none()
    && arguments.password_fd.is_none()
    && arguments.password_pipe.is_none()
    && arguments.password_socket.is_none()
  {
    return Ok(());
  }
  if arguments.destination.is_none()
    || !matches!(
      arguments.command,
      Command::Shell { .. } | Command::Exec { .. }
    )
  {
    return Err(channel::invalid(
      "Per-invocation SSH options require --destination with shell or exec",
    ));
  }
  if arguments.user.as_ref().is_some_and(|user| {
    user.is_empty()
      || user.starts_with('-')
      || user
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '@')
  }) {
    return Err(channel::invalid("Invalid SSH user"));
  }
  Ok(())
}

pub async fn prepare(
  arguments: &mut Arguments,
  target: &mut ConnectionTargetDto,
) -> io::Result<Option<Arc<CallerAuth>>> {
  if let ConnectionTargetDto::Ssh { user, port, .. } = target {
    if arguments.user.is_some() {
      user.clone_from(&arguments.user);
    }
    if arguments.port.is_some() {
      *port = arguments.port;
    }
  }
  let secret = if arguments.password_fd.is_some() {
    let file = arguments
      .password_input
      .take()
      .ok_or_else(|| channel::invalid("Password descriptor was not inherited"))?;
    Some(channel::inherited(file).await?)
  } else if let Some(path) = &arguments.password_pipe {
    Some(channel::pipe(path).await?)
  } else if let Some(path) = &arguments.password_socket {
    Some(channel::socket(path).await?)
  } else {
    None
  };
  if secret.is_none() && arguments.identity_file.is_none() {
    return Ok(None);
  }
  // Consume/close the credential channel before even the config-query process.
  let host_options = host_options(target).await?;
  let (credential, task) = if let Some(secret) = secret {
    let listener = channel::Listener::new()?;
    let token = uuid::Uuid::new_v4().to_string();
    let credential = Credential::Password {
      endpoint: listener.path.clone(),
      token: token.clone(),
    };
    let task = tokio::spawn(async move {
      // The only retained copy is zeroized when this invocation's bridge ends.
      while let Ok(mut stream) = listener.accept().await {
        let response = async {
          let mut supplied = [0_u8; 36];
          stream.read_exact(&mut supplied).await?;
          if supplied != token.as_bytes() {
            return Err(channel::invalid("Invalid askpass token"));
          }
          stream.write_all(&secret).await?;
          stream.shutdown().await
        };
        let _ = tokio::time::timeout(channel::TIMEOUT, response).await;
      }
    });
    (credential, Some(task))
  } else {
    let path = arguments
      .identity_file
      .as_ref()
      .ok_or_else(|| channel::invalid("Missing identity file"))?;
    let path = path
      .canonicalize()
      .map_err(|_| channel::invalid("Could not open the supplied identity file"))?;
    if !path.is_file() {
      return Err(channel::invalid("Identity must be a private-key file"));
    }
    (Credential::Key(path), None)
  };
  Ok(Some(Arc::new(CallerAuth {
    authentication: Authentication {
      credential,
      host_options,
      prompt_program: std::env::current_exe()?,
      allow_host_key_confirmation: host_trust::has_controlling_terminal(),
    },
    task,
  })))
}

async fn host_options(target: &ConnectionTargetDto) -> io::Result<Vec<std::ffi::OsString>> {
  let text = query_config(target, false).await?;
  let mut defaults = None;
  let mut options = Vec::new();
  for line in text.lines() {
    let Some((name, value)) = line.split_once(' ') else {
      continue;
    };
    if matches!(name, "userknownhostsfile" | "globalknownhostsfile") && ambiguous_paths(value) {
      // OpenSSH -G does not quote individual paths. Do not accidentally turn
      // a path containing spaces into several different trust stores. Default
      // paths can be inherited losslessly, including Windows profiles with spaces.
      if defaults.is_none() {
        defaults = Some(query_config(target, true).await?);
      }
      let unchanged = defaults.as_deref().is_some_and(|text| {
        text
          .lines()
          .any(|line| line.split_once(' ') == Some((name, value)))
      });
      if unchanged {
        continue;
      }
      return Err(channel::invalid(
        "Custom known-hosts paths containing whitespace cannot be resolved unambiguously; use an unambiguous absolute path",
      ));
    }
    // IdentityFile is additive in OpenSSH, so overriding it cannot exclude
    // configured identities. Resolve only endpoint and host-key policy, then
    // run with -F none. Authentication and proxy config cannot re-enter.
    if matches!(
      name,
      "hostname"
        | "user"
        | "port"
        | "hostkeyalias"
        | "userknownhostsfile"
        | "globalknownhostsfile"
        | "stricthostkeychecking"
        | "checkhostip"
        | "verifyhostkeydns"
        | "updatehostkeys"
        | "hashknownhosts"
        | "hostkeyalgorithms"
        | "fingerprinthash"
        | "casignaturealgorithms"
        | "revokedhostkeys"
        | "knownhostscommand"
    ) {
      if value.chars().any(char::is_control) {
        return Err(channel::invalid("Invalid SSH host-key settings"));
      }
      let value = if name == "revokedhostkeys" {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
      } else {
        value.into()
      };
      options.extend(["-o".into(), format!("{name}={value}").into()]);
    }
    if matches!(name, "proxycommand" | "proxyjump") && value != "none" {
      return Err(channel::invalid(
        "Caller-owned credentials require a direct SSH destination, without configured proxy authentication",
      ));
    }
  }
  Ok(options)
}

fn ambiguous_paths(value: &str) -> bool {
  value.contains(char::is_whitespace)
    && value.split_whitespace().any(|path| {
      !(path.starts_with('/')
        || (path.as_bytes().get(1) == Some(&b':')
          && path
            .as_bytes()
            .get(2)
            .is_some_and(|byte| matches!(byte, b'/' | b'\\'))))
    })
}

async fn query_config(target: &ConnectionTargetDto, defaults: bool) -> io::Result<String> {
  let ssh = target
    .to_ssh_target()
    .map_err(|_| channel::invalid("Invalid SSH destination"))?;
  let mut command = tokio::process::Command::new("ssh");
  if defaults {
    command.args(["-F", "none"]);
  }
  command
    .args([
      "-G",
      "-o",
      "PermitLocalCommand=no",
      "-o",
      "RemoteCommand=none",
    ])
    .args(crate::connection::target_arguments(&ssh).map_err(io::Error::other)?)
    .arg("--")
    .arg(&ssh.destination)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    .kill_on_drop(true);
  let mut child = command.spawn()?;
  let mut output = Vec::new();
  let read = async {
    child
      .stdout
      .as_mut()
      .ok_or_else(|| channel::invalid("Missing SSH configuration output"))?
      .take(128 * 1024 + 1)
      .read_to_end(&mut output)
      .await?;
    if output.len() > 128 * 1024 {
      return Err(channel::invalid(
        "SSH configuration exceeds the output limit",
      ));
    }
    if !child.wait().await?.success() {
      return Err(channel::invalid(
        "Could not resolve the temporary SSH endpoint",
      ));
    }
    Ok::<_, io::Error>(())
  };
  tokio::time::timeout(channel::TIMEOUT, read)
    .await
    .map_err(|_| channel::invalid("SSH configuration lookup timed out"))??;
  String::from_utf8(output).map_err(|_| channel::invalid("Invalid SSH configuration output"))
}

pub async fn askpass() -> io::Result<()> {
  let prompt = std::env::args().nth(1).unwrap_or_default();
  // OpenSSH's first-use host prompt uses RP_ECHO, not the separate
  // SSH_ASKPASS_PROMPT=confirm permission mechanism (e.g. security keys).
  if std::env::var_os("SSH_ASKPASS_PROMPT").is_some() {
    return Err(channel::invalid(
      "This SSH authentication prompt is not supported with a supplied credential",
    ));
  }
  if let Some(fingerprint) = host_trust::fingerprint(&prompt) {
    if std::env::var("CTL_CALLER_CONFIRM_TTY").ok().as_deref() != Some("1") {
      return Err(host_trust::terminal_required());
    }
    let response = host_trust::confirm(&prompt, fingerprint)?;
    return writeln!(std::io::stdout().lock(), "{response}");
  }
  if prompt.starts_with("The authenticity of host '") {
    return Err(channel::invalid(
      "Unrecognized SSH host-key confirmation format; refusing to answer with a supplied credential",
    ));
  }
  if !prompt.trim_end().ends_with("'s password:")
    || prompt.contains(['\r', '\n'])
    || std::env::var_os("CTL_CALLER_ENDPOINT").is_none()
  {
    return Err(channel::invalid(
      "Only the supplied SSH credential may authenticate this connection; passphrase, PIN and other prompts are disabled",
    ));
  }
  let endpoint = PathBuf::from(
    std::env::var_os("CTL_CALLER_ENDPOINT")
      .ok_or_else(|| channel::invalid("Missing askpass endpoint"))?,
  );
  let token =
    std::env::var("CTL_CALLER_TOKEN").map_err(|_| channel::invalid("Missing askpass token"))?;
  if token.len() != 36 {
    return Err(channel::invalid("Invalid askpass token"));
  }
  #[cfg(unix)]
  let mut stream = tokio::net::UnixStream::connect(endpoint).await?;
  #[cfg(windows)]
  let mut stream = tokio::net::windows::named_pipe::ClientOptions::new().open(endpoint)?;
  stream.write_all(token.as_bytes()).await?;
  let secret = channel::password(stream).await?;
  let mut output = std::io::stdout().lock();
  output.write_all(&secret)?;
  output.write_all(b"\n")
}
