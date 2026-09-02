use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use zeroize::Zeroizing;

use super::HelperRequest;

/// The same executable is invoked by OpenSSH as an askpass helper before Tauri starts.
#[must_use]
pub fn helper_exit_code() -> Option<i32> {
  if std::env::var("CTL_SSH_ASKPASS").ok().as_deref() != Some("1") {
    return None;
  }
  Some(i32::from(run().is_err()))
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
  let socket = std::env::var("CTL_SSH_ASKPASS_SOCKET")?;
  let token = std::env::var("CTL_SSH_ASKPASS_TOKEN")?;
  let message = std::env::args().nth(1).ok_or("missing prompt")?;
  if message.len() > 8192 {
    return Err("prompt too long".into());
  }
  let confirm = std::env::var("SSH_ASKPASS_PROMPT").ok().as_deref() == Some("confirm")
    || is_host_confirmation(&message);
  let mut stream = UnixStream::connect(socket)?;
  stream.set_read_timeout(Some(Duration::from_secs(125)))?;
  stream.set_write_timeout(Some(Duration::from_secs(5)))?;
  serde_json::to_writer(
    &mut stream,
    &HelperRequest {
      token,
      message,
      confirm,
    },
  )?;
  stream.write_all(b"\n")?;
  let mut line = Zeroizing::new(String::new());
  BufReader::new(stream.take(16_384)).read_line(&mut line)?;
  let response = serde_json::from_str::<Option<String>>(&line)?
    .map(Zeroizing::new)
    .ok_or("cancelled")?;
  std::io::stdout().write_all(response.as_bytes())?;
  std::io::stdout().write_all(b"\n")?;
  Ok(())
}

fn is_host_confirmation(message: &str) -> bool {
  // OpenSSH sshconnect.c uses RP_ECHO for host trust, not RP_ASK_PERMISSION,
  // so SSH_ASKPASS_PROMPT is unset. Recognize only its explicit yes/no suffixes.
  let message = message.trim_end();
  message.ends_with("Are you sure you want to continue connecting (yes/no/[fingerprint])?")
    || message.ends_with("Are you sure you want to continue connecting (yes/no)?")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn recognizes_openssh_host_questions_without_treating_passwords_as_confirmation() {
    assert!(is_host_confirmation(
      "The authenticity of host cannot be established.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? "
    ));
    assert!(is_host_confirmation(
      "Warning: conflicting IP key\nAre you sure you want to continue connecting (yes/no)? "
    ));
    assert!(!is_host_confirmation("Password:"));
    assert!(!is_host_confirmation("Verification code:"));
  }
}
