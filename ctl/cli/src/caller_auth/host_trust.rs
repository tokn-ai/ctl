//! First-use host trust is a terminal decision, never a password-bridge request.

use std::fs::{File, OpenOptions};
use std::io;

const MAX_RESPONSE_BYTES: usize = 256;

/// Recognize only OpenSSH's full, locally generated first-use host prompt.
/// Other prompt types must not gain access to a terminal or a supplied secret.
pub(super) fn fingerprint(prompt: &str) -> Option<&str> {
  if prompt.len() > 16 * 1024
    || !prompt.starts_with("The authenticity of host '")
    || !prompt.lines().next()?.ends_with("can't be established.")
    || ![
      "Are you sure you want to continue connecting (yes/no/[fingerprint])?",
      "Are you sure you want to continue connecting (yes/no)?",
    ]
    .iter()
    .any(|question| prompt.trim_end().ends_with(question))
    || prompt
      .chars()
      .any(|character| character.is_control() && !matches!(character, '\r' | '\n' | '\t'))
  {
    return None;
  }
  let (_, fingerprint) = prompt.lines().nth(1)?.split_once(" key fingerprint is ")?;
  let fingerprint = fingerprint.strip_suffix('.')?;
  if fingerprint.starts_with("SHA256:") || fingerprint.starts_with("MD5:") {
    let (_, value) = fingerprint.split_once(':')?;
    if !value.is_empty()
      && value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"+/=:".contains(&byte))
    {
      return Some(fingerprint);
    }
  }
  None
}

struct Terminal {
  input: File,
  output: File,
}

impl Terminal {
  fn open() -> io::Result<Self> {
    #[cfg(unix)]
    let (input, output) = {
      let input = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
      let output = input.try_clone()?;
      (input, output)
    };
    #[cfg(windows)]
    let (input, output) = (
      OpenOptions::new().read(true).open("CONIN$")?,
      OpenOptions::new().write(true).open("CONOUT$")?,
    );
    Ok(Self { input, output })
  }
}

pub(super) fn has_controlling_terminal() -> bool {
  Terminal::open().is_ok()
}

pub(super) fn terminal_required() -> io::Error {
  io::Error::new(
    io::ErrorKind::PermissionDenied,
    "SSH host-key confirmation requires the caller's controlling terminal; establish host trust before noninteractive use",
  )
}

pub(super) fn confirm(prompt: &str, fingerprint: &str) -> io::Result<&'static str> {
  let mut terminal = Terminal::open().map_err(|_| terminal_required())?;
  read_confirmation(
    prompt,
    fingerprint,
    &mut terminal.input,
    &mut terminal.output,
  )
}

fn read_confirmation(
  prompt: &str,
  fingerprint: &str,
  input: &mut impl io::Read,
  output: &mut impl io::Write,
) -> io::Result<&'static str> {
  output.write_all(prompt.as_bytes())?;
  output.flush()?;
  loop {
    // Read exactly through this answer, leaving terminal typeahead after its
    // newline available to SSH. Command stdin is a separate, untouched stream.
    let mut response = Vec::new();
    loop {
      let mut byte = [0];
      if input.read(&mut byte)? == 0 {
        return Err(io::Error::new(
          io::ErrorKind::UnexpectedEof,
          "SSH host-key confirmation ended without approval",
        ));
      }
      if byte[0] == b'\n' {
        break;
      }
      if response.len() == MAX_RESPONSE_BYTES {
        return Err(io::Error::new(
          io::ErrorKind::InvalidInput,
          "SSH host-key confirmation response is too long",
        ));
      }
      response.push(byte[0]);
    }
    let response = std::str::from_utf8(&response)
      .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid host-key confirmation"))?
      .trim();
    if response.eq_ignore_ascii_case("yes") || response == fingerprint {
      return Ok("yes");
    }
    if response.eq_ignore_ascii_case("no") {
      return Ok("no");
    }
    output.write_all(b"Please type 'yes', 'no' or the fingerprint: ")?;
    output.flush()?;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const FINGERPRINT: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

  fn prompt() -> String {
    format!(
      "The authenticity of host 'host (192.0.2.1)' can't be established.\nED25519 key fingerprint is {FINGERPRINT}.\nThis key is not known by any other names.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? "
    )
  }

  #[test]
  fn only_the_complete_first_use_prompt_is_a_host_trust_request() {
    assert_eq!(fingerprint(&prompt()), Some(FINGERPRINT));
    assert_eq!(
      fingerprint(&prompt().replace("(yes/no/[fingerprint])", "(yes/no)")),
      Some(FINGERPRINT)
    );
    assert_eq!(
      fingerprint(&prompt().replace(FINGERPRINT, "MD5:aa:bb:cc")),
      Some("MD5:aa:bb:cc")
    );
    for prompt in [
      "user@host's password: ".into(),
      "Enter passphrase for key '/key': ".into(),
      "Enter PIN for authenticator: ".into(),
      "Are you sure you want to continue connecting (yes/no)? ".into(),
      prompt().replace(" key fingerprint is ", " password is "),
      prompt().replace(FINGERPRINT, "unrecognized:fingerprint"),
      prompt().replace("ED25519", "\u{1b}[31mED25519"),
    ] {
      assert_eq!(fingerprint(&prompt), None, "{prompt:?}");
    }
  }

  #[test]
  fn confirmation_reads_an_explicit_answer_and_preserves_terminal_typeahead() {
    for answer in ["yes", "YES", FINGERPRINT] {
      let bytes = format!("{answer}\r\ncommand typeahead\n");
      let mut input = bytes.as_bytes();
      let mut output = Vec::new();
      assert_eq!(
        read_confirmation(&prompt(), FINGERPRINT, &mut input, &mut output).unwrap(),
        "yes"
      );
      assert_eq!(input, b"command typeahead\n");
      assert_eq!(output, prompt().as_bytes());
    }
    let mut output = Vec::new();
    assert_eq!(
      read_confirmation(&prompt(), FINGERPRINT, &mut b"no\n".as_slice(), &mut output).unwrap(),
      "no"
    );
    let mut output = Vec::new();
    assert_eq!(
      read_confirmation(
        &prompt(),
        FINGERPRINT,
        &mut b"maybe\nyes\n".as_slice(),
        &mut output
      )
      .unwrap(),
      "yes"
    );
    assert!(output.ends_with(b"Please type 'yes', 'no' or the fingerprint: "));
  }

  #[test]
  fn missing_or_unbounded_answers_never_approve_host_trust() {
    for bytes in [Vec::new(), vec![b'x'; MAX_RESPONSE_BYTES + 1]] {
      assert!(
        read_confirmation(
          &prompt(),
          FINGERPRINT,
          &mut bytes.as_slice(),
          &mut Vec::new()
        )
        .is_err()
      );
    }
  }
}
