//! OpenSSH askpass and the invocation-local credential channel.
pub mod channel;
mod host_trust;

pub use host_trust::has_controlling_terminal;
use std::io::Write as _;
use std::{io, path::PathBuf};
use tokio::io::AsyncWriteExt as _;

pub(crate) const ASKPASS_MODE: &str = "CTL_CALLER_ASKPASS";

pub(crate) async fn askpass() -> io::Result<()> {
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
