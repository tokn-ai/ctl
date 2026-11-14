//! Validate and render self-contained `OpenVPN` client profiles. The container
//! owns its TUN device, lifecycle hooks, diagnostics, and proxy configuration.

use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

/// Keep the profile below the local and remote VPN frame limits even after
/// JSON escaping. Only tabs and line endings may appear as control characters.
pub const MAX_CONFIG_BYTES: usize = 24 * 1024;

// OpenVPN reads inline data with fgets into a 256-byte buffer, including NUL.
const MAX_LINE_BYTES: usize = 254;
const INLINE_OPTIONS: &[&str] = &[
  "ca",
  "cert",
  "key",
  "extra-certs",
  "pkcs12",
  "tls-auth",
  "tls-crypt",
  "tls-crypt-v2",
  "peer-fingerprint",
  "auth-user-pass",
];

/// Check that a profile can be started without prompts, external files or hooks.
///
/// # Errors
/// Returns a field or line number without quoting profile contents.
pub fn validate(
  config: &str,
  username: Option<&str>,
  password: Option<&str>,
) -> Result<(), String> {
  render(config, username, password).map(|_| ())
}

/// Produce the exact configuration consumed by the managed `OpenVPN` container.
///
/// # Errors
/// Returns a sanitized diagnostic if the profile or credentials are unsupported.
pub fn render(
  config: &str,
  username: Option<&str>,
  password: Option<&str>,
) -> Result<Zeroizing<String>, String> {
  let mut profile = parse(config)?;
  match (username, password) {
    (Some(username), Some(password)) if !profile.inline_auth => {
      for value in [username, password] {
        if !valid_credential(value) {
          return Err(
            "OpenVPN credentials must be nonempty single lines of at most 254 bytes".into(),
          );
        }
      }
      profile.output.push_str("<auth-user-pass>\n");
      profile.output.push_str(username);
      profile.output.push('\n');
      profile.output.push_str(password);
      profile.output.push_str("\n</auth-user-pass>\n");
    }
    (None, None) if !profile.needs_auth || profile.inline_auth => {}
    (Some(_), Some(_)) => {
      return Err(
        "Use either inline OpenVPN credentials or the username and password fields".into(),
      );
    }
    _ => return Err("Provide both an OpenVPN username and password".into()),
  }
  Ok(profile.output)
}

/// Extract noncredential settings for matching previously launched containers.
/// Inline data, which can contain private keys and passwords, stays private.
///
/// # Errors
/// Returns a sanitized diagnostic for unsupported configuration.
pub fn public_settings(config: &str) -> Result<Vec<String>, String> {
  parse(config).map(|profile| profile.settings)
}

#[derive(Default)]
#[expect(
  clippy::struct_excessive_bools,
  reason = "independent directives can appear in any order"
)]
struct Profile {
  output: Zeroizing<String>,
  settings: Vec<String>,
  needs_auth: bool,
  inline_auth: bool,
  client_mode: bool,
  has_remote: bool,
  server_usage: bool,
  fingerprint: bool,
}

impl Profile {
  fn push_option(&mut self, option: String) {
    self.output.push_str(&option);
    self.output.push('\n');
    self.settings.push(option);
  }
}

fn parse(config: &str) -> Result<Profile, String> {
  if config.len() > MAX_CONFIG_BYTES {
    return Err("OpenVPN profile must be at most 24 KiB".into());
  }
  let config = config.strip_prefix('\u{feff}').unwrap_or(config);
  let mut lines = config.lines().enumerate();
  let mut profile = Profile::default();
  profile.output.push_str("client\n");
  let mut inline = std::collections::HashSet::new();
  while let Some((index, line)) = lines.next() {
    let line_number = index + 1;
    validate_line(line, line_number)?;
    let trimmed = line.trim_matches([' ', '\t']);
    if trimmed.is_empty() || trimmed.starts_with(['#', ';']) {
      continue;
    }
    if let Some(tag) = trimmed.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
      if !INLINE_OPTIONS.contains(&tag) || !inline.insert(tag) {
        return Err(unsupported(line_number));
      }
      read_inline(&mut profile, &mut lines, tag, line_number)?;
    } else {
      let tokens = words(trimmed).ok_or_else(|| unsupported(line_number))?;
      add_option(&mut profile, &tokens, line_number)?;
    }
  }
  if !profile.client_mode || !profile.has_remote {
    return Err("OpenVPN profile needs client mode and at least one remote server".into());
  }
  if !(inline.contains("ca") || inline.contains("pkcs12") || profile.fingerprint) {
    return Err("OpenVPN profile needs an inline CA, PKCS#12 bundle, or server fingerprint".into());
  }
  if !profile.server_usage && !profile.fingerprint {
    profile.push_option("remote-cert-tls server".into());
  }
  Ok(profile)
}

fn validate_line(line: &str, number: usize) -> Result<(), String> {
  if line.len() > MAX_LINE_BYTES || line.chars().any(|c| c.is_control() && c != '\t') {
    return Err(format!(
      "OpenVPN profile line {number} is too long or contains a control character"
    ));
  }
  Ok(())
}

fn read_inline<'a>(
  profile: &mut Profile,
  lines: &mut impl Iterator<Item = (usize, &'a str)>,
  tag: &str,
  line_number: usize,
) -> Result<(), String> {
  let closing = format!("</{tag}>");
  profile.output.push('<');
  profile.output.push_str(tag);
  profile.output.push_str(">\n");
  let trust_start = profile.output.len();
  for (count, (index, line)) in lines.by_ref().enumerate() {
    validate_line(line, index + 1)?;
    let trimmed = line.trim_matches([' ', '\t']);
    // OpenVPN ends an inline block on a closing-tag prefix, so reject
    // ambiguous delimiters and emit exactly one canonical closing line.
    if trimmed == closing {
      if count == 0 || (tag == "auth-user-pass" && count != 2) {
        return Err(unsupported(line_number));
      }
      // These blocks contain public server trust, not private key material.
      // Changes to trust must prevent adoption of an already-running tunnel.
      if matches!(tag, "ca" | "peer-fingerprint" | "extra-certs") {
        let digest = Sha256::digest(&profile.output.as_bytes()[trust_start..]);
        profile.settings.push(format!("<{tag}> {digest:x}"));
      } else {
        profile.settings.push(format!("<{tag}>"));
      }
      profile.output.push_str(&closing);
      profile.output.push('\n');
      profile.inline_auth |= tag == "auth-user-pass";
      profile.fingerprint |= tag == "peer-fingerprint";
      return Ok(());
    }
    if trimmed.starts_with('<')
      || (tag == "auth-user-pass" && !valid_credential(line))
      || (tag == "peer-fingerprint" && !valid_fingerprint(trimmed))
    {
      return Err(unsupported(index + 1));
    }
    profile.output.push_str(line);
    profile.output.push('\n');
  }
  Err(format!(
    "OpenVPN profile has an unclosed inline block at line {line_number}"
  ))
}

fn valid_credential(value: &str) -> bool {
  !value.trim().is_empty()
    && value.len() <= MAX_LINE_BYTES
    && !value.trim_start().starts_with('<')
    && !value.chars().any(char::is_control)
}

// Read only unescaped words and simple quoted arguments. Re-emit them with a
// single spelling so no quoting or comment rule can change directive parsing.
fn words(mut line: &str) -> Option<Vec<&str>> {
  let mut tokens = Vec::new();
  while !line.is_empty() && !line.starts_with(['#', ';']) {
    let (token, remaining) =
      if let Some(quote) = line.chars().next().filter(|c| ['\'', '"'].contains(c)) {
        line[1..].split_once(quote)?
      } else {
        line.split_at(line.find([' ', '\t']).unwrap_or(line.len()))
      };
    if token.is_empty()
      || !token
        .bytes()
        .all(|byte| byte == b' ' || byte.is_ascii_graphic())
      || token.contains(['\'', '"', '\\', '<', '>', '#', ';'])
      || !(remaining.is_empty() || remaining.starts_with([' ', '\t']))
    {
      return None;
    }
    tokens.push(token);
    line = remaining.trim_start_matches([' ', '\t']);
  }
  Some(tokens)
}

fn add_option(profile: &mut Profile, words: &[&str], number: usize) -> Result<(), String> {
  match words {
    ["client" | "tls-client"] => profile.client_mode = true,
    ["pull" | "block-outside-dns"]
    | ["setenv", "opt", "block-outside-dns"]
    | ["ignore-unknown-option", "block-outside-dns"]
    | ["dev-type", "tun"]
    | ["verb" | "mute", _] => {}
    ["dev", device]
      if device
        .strip_prefix("tun")
        .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit())) => {}
    ["auth-user-pass"] => profile.needs_auth = true,
    ["remote", host, rest @ ..] if rest.len() <= 2 && valid_host(host) => {
      profile.has_remote = true;
      emit(profile, words, number)?;
    }
    ["remote-cert-tls", "server"] => {
      profile.server_usage = true;
      emit(profile, words, number)?;
    }
    ["peer-fingerprint", hash] if valid_fingerprint(hash) => {
      profile.fingerprint = true;
      emit(profile, words, number)?;
    }
    [option, args @ ..] if allowed_arguments(option, args.len()) => emit(profile, words, number)?,
    _ => return Err(unsupported(number)),
  }
  Ok(())
}

fn allowed_arguments(option: &str, count: usize) -> bool {
  match option {
    "auth-nocache"
    | "float"
    | "nobind"
    | "persist-key"
    | "persist-tun"
    | "ping-timer-rem"
    | "remote-random"
    | "remote-random-hostname"
    | "route-nopull"
    | "disable-dco" => count == 0,
    "allow-compression"
    | "auth"
    | "auth-retry"
    | "cipher"
    | "connect-retry-max"
    | "connect-timeout"
    | "data-ciphers"
    | "data-ciphers-fallback"
    | "inactive"
    | "key-direction"
    | "link-mtu"
    | "ping"
    | "ping-exit"
    | "ping-restart"
    | "port"
    | "proto"
    | "reneg-sec"
    | "resolv-retry"
    | "route-metric"
    | "tls-cipher"
    | "tls-ciphersuites"
    | "tls-timeout"
    | "tun-mtu"
    | "connect-retry"
    | "tls-version-min"
    | "tls-version-max" => count == 1 || (option == "connect-retry" && count == 2),
    "explicit-exit-notify" => count <= 1,
    "mssfix" => count <= 2,
    "keepalive" | "pull-filter" | "dhcp-option" => count == 2,
    "verify-x509-name" | "route-ipv6" => (1..=3).contains(&count),
    "route" => (1..=4).contains(&count),
    "redirect-gateway" => count <= 6,
    _ => false,
  }
}

fn valid_host(host: &str) -> bool {
  !host.starts_with('-')
    && host.bytes().all(|byte| {
      byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':' | b'[' | b']')
    })
}

fn valid_fingerprint(hash: &str) -> bool {
  hash.len() == 95
    && hash
      .split(':')
      .all(|octet| octet.len() == 2 && octet.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn emit(profile: &mut Profile, tokens: &[&str], number: usize) -> Result<(), String> {
  let mut option = String::new();
  for token in tokens {
    if !option.is_empty() {
      option.push(' ');
    }
    let quote = token.contains(' ');
    if quote {
      option.push('"');
    }
    option.push_str(token);
    if quote {
      option.push('"');
    }
  }
  validate_line(&option, number)?;
  profile.push_option(option);
  Ok(())
}

fn unsupported(number: usize) -> String {
  format!(
    "Unsupported OpenVPN option or inline data at line {number}. Use a self-contained TUN client profile."
  )
}

#[cfg(test)]
mod tests;
