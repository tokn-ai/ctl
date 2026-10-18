//! Caller-owned credentials never enter ctld, saved-host storage or audit metadata.
mod channel;
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
      program: std::env::current_exe()?,
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
    },
    task,
  })))
}

async fn host_options(target: &ConnectionTargetDto) -> io::Result<Vec<std::ffi::OsString>> {
  let ssh = target
    .to_ssh_target()
    .map_err(|_| channel::invalid("Invalid SSH destination"))?;
  let mut command = tokio::process::Command::new("ssh");
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
    .kill_on_drop(true);
  let output = tokio::time::timeout(channel::TIMEOUT, command.output())
    .await
    .map_err(|_| channel::invalid("SSH configuration lookup timed out"))??;
  if !output.status.success() || output.stdout.len() > 128 * 1024 {
    return Err(channel::invalid(
      "Could not resolve the temporary SSH endpoint",
    ));
  }
  let text = std::str::from_utf8(&output.stdout)
    .map_err(|_| channel::invalid("Invalid SSH configuration output"))?;
  let mut options = Vec::new();
  for line in text.lines() {
    let Some((name, value)) = line.split_once(' ') else {
      continue;
    };
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
        | "casignaturealgorithms"
        | "revokedhostkeys"
        | "knownhostscommand"
    ) {
      if value.chars().any(char::is_control) {
        return Err(channel::invalid("Invalid SSH host-key settings"));
      }
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

pub async fn askpass() -> io::Result<()> {
  let prompt = std::env::args().nth(1).unwrap_or_default();
  if std::env::var("SSH_ASKPASS_PROMPT").ok().as_deref() == Some("confirm")
    || !prompt
      .to_ascii_lowercase()
      .trim_end()
      .ends_with("password:")
  {
    return Err(channel::invalid(
      "Only password authentication is supported",
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
