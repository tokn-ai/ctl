//! Resolve only the static endpoint options needed by a greeting probe.
//!
//! Invoking `ssh -G` is not passive: `Match exec` can run arbitrary commands.
//! This evaluator deliberately declines configuration it cannot reproduce.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use ctl_ipc::SshTarget;

use super::{SshReachability, SshReachabilityReason, SshReachabilityState};

const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_CONFIG_FILES: usize = 128;
const MAX_INCLUDE_DEPTH: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Endpoint {
  pub host: String,
  pub port: u16,
}

pub(super) async fn resolve(target: &SshTarget) -> Result<Endpoint, SshReachability> {
  let target = target.clone();
  tokio::task::spawn_blocking(move || {
    let paths = ConfigPaths::discover()?;
    resolve_with_paths(&target, &paths)
  })
  .await
  .map_err(|_| check_failed())?
}

struct ConfigPaths {
  home: PathBuf,
  user_directory: PathBuf,
  system_directory: PathBuf,
}

impl ConfigPaths {
  fn discover() -> Result<Self, SshReachability> {
    #[cfg(unix)]
    {
      let home = dirs::home_dir().ok_or_else(check_failed)?;
      Ok(Self {
        user_directory: home.join(".ssh"),
        home,
        system_directory: PathBuf::from("/etc/ssh"),
      })
    }
    // Win32 OpenSSH has different Include/glob and installation-path
    // semantics. Do not risk missing a proxy rule and probing directly.
    #[cfg(not(unix))]
    Err(unsupported(
      "Static SSH configuration inspection is not supported on this platform.",
    ))
  }
}

fn resolve_with_paths(
  target: &SshTarget,
  paths: &ConfigPaths,
) -> Result<Endpoint, SshReachability> {
  // Match exactly the argument selection in ctld's append_target_arguments.
  let destination = target
    .ssh_config_alias
    .as_deref()
    .unwrap_or_else(|| target.hostname.as_deref().unwrap_or(&target.destination));
  let match_host = destination_host(destination)?;
  let explicit_hostname = target
    .ssh_config_alias
    .as_ref()
    .and(target.hostname.as_deref());
  let mut evaluator = Evaluator {
    paths,
    match_host: &match_host,
    values: HashMap::new(),
    proxy_command: None,
    proxy_jump: None,
    bytes: 0,
    files: 0,
  };
  if let Some(hostname) = explicit_hostname {
    evaluator.values.insert("hostname".into(), hostname.into());
  }
  if let Some(port) = target.port {
    evaluator.values.insert("port".into(), port.to_string());
  }
  if !target.gateways.is_empty() {
    // Route preflight owns app gateway support. The explicit route takes
    // precedence over both proxy options in user/system configuration.
    evaluator.proxy_command = Some(false);
    evaluator.proxy_jump = Some(false);
  }
  evaluator.visit(&paths.user_directory.join("config"), true, 0)?;
  evaluator.visit(&paths.system_directory.join("ssh_config"), false, 0)?;
  evaluator.endpoint()
}

struct Evaluator<'a> {
  paths: &'a ConfigPaths,
  match_host: &'a str,
  values: HashMap<String, String>,
  proxy_command: Option<bool>,
  proxy_jump: Option<bool>,
  bytes: usize,
  files: usize,
}

impl Evaluator<'_> {
  fn visit(&mut self, path: &Path, user_config: bool, depth: usize) -> Result<(), SshReachability> {
    if depth > MAX_INCLUDE_DEPTH || self.files >= MAX_CONFIG_FILES {
      return Err(unsupported(
        "SSH configuration exceeds the safe inspection limit.",
      ));
    }
    let metadata = match std::fs::metadata(path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
      Err(_) => return Err(check_failed()),
    };
    if !metadata.is_file() {
      return Err(unsupported("SSH configuration is not a regular file."));
    }
    let remaining = MAX_CONFIG_BYTES.saturating_sub(self.bytes);
    if metadata.len() > remaining as u64 {
      return Err(unsupported(
        "SSH configuration exceeds the safe inspection limit.",
      ));
    }
    let mut contents = String::new();
    File::open(path)
      .and_then(|file| {
        file
          .take(remaining as u64 + 1)
          .read_to_string(&mut contents)
      })
      .map_err(|_| check_failed())?;
    self.bytes += contents.len();
    self.files += 1;
    if self.bytes > MAX_CONFIG_BYTES {
      return Err(unsupported(
        "SSH configuration exceeds the safe inspection limit.",
      ));
    }
    // Each included file starts with the containing active Host condition.
    // Inactive Includes are never visited; an included Host cannot activate one.
    let mut active = true;
    for line in contents.lines() {
      let Some((keyword, arguments)) = parse_directive(line)? else {
        continue;
      };
      match keyword.as_str() {
        "host" => active = host_matches(self.match_host, &arguments)?,
        "match" => {
          return Err(unsupported(
            "SSH Match rules require a connection-specific evaluation.",
          ));
        }
        "include" if active => {
          if arguments.is_empty() {
            return Err(unsupported("SSH Include has no path."));
          }
          for pattern in &arguments {
            for included_path in self.include_paths(pattern, user_config)? {
              // OpenSSH restores the surrounding Host condition after every
              // included file, including files from the same wildcard.
              self.visit(&included_path, user_config, depth + 1)?;
            }
          }
        }
        _ if !active => {}
        "proxycommand" => {
          if self.proxy_command.is_none() {
            // OpenSSH retains the shell command verbatim; a quoted "none"
            // is a command, not the sentinel that disables proxying.
            self.proxy_command = Some(!directive_arguments(line).eq_ignore_ascii_case("none"));
          }
        }
        "proxyjump" => {
          if directive_arguments(line).eq_ignore_ascii_case("none") {
            self.proxy_jump.get_or_insert(false);
          } else if self.proxy_command.is_none() && self.proxy_jump.is_none() {
            self.proxy_jump = Some(true);
            self.proxy_command = Some(false);
          }
        }
        "hostname"
        | "port"
        | "bindaddress"
        | "bindinterface"
        | "addressfamily"
        | "canonicalizehostname"
        | "refuseconnection" => {
          let value = one_argument(&arguments)?;
          self.values.entry(keyword).or_insert_with(|| value.into());
        }
        _ if is_non_routing_option(&keyword) => {}
        _ => {
          return Err(unsupported(
            "SSH configuration contains an unsupported directive.",
          ));
        }
      }
    }
    Ok(())
  }

  fn include_paths(
    &self,
    pattern: &str,
    user_config: bool,
  ) -> Result<Vec<PathBuf>, SshReachability> {
    if pattern.contains(['%', '$', '\\']) || pattern.contains("**") || pattern.is_empty() {
      return Err(unsupported(
        "SSH Include uses an unsupported path expansion.",
      ));
    }
    let path = if let Some(relative) = pattern.strip_prefix("~/") {
      if !user_config {
        return Err(unsupported(
          "System SSH Include cannot use a home-relative path.",
        ));
      }
      self.paths.home.join(relative)
    } else if pattern.starts_with('~') {
      return Err(unsupported(
        "SSH Include uses an unsupported home expansion.",
      ));
    } else if Path::new(pattern).is_absolute() {
      PathBuf::from(pattern)
    } else if user_config {
      self.paths.user_directory.join(pattern)
    } else {
      self.paths.system_directory.join(pattern)
    };
    let pattern = path.to_str().ok_or_else(check_failed)?;
    let entries = glob::glob_with(
      pattern,
      glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: true,
      },
    )
    .map_err(|_| unsupported("SSH Include uses an unsupported glob pattern."))?;
    let mut paths = Vec::new();
    for entry in entries {
      paths.push(entry.map_err(|_| check_failed())?);
      if paths.len() + self.files > MAX_CONFIG_FILES {
        return Err(unsupported(
          "SSH configuration exceeds the safe inspection limit.",
        ));
      }
    }
    paths.sort();
    Ok(paths)
  }

  fn endpoint(self) -> Result<Endpoint, SshReachability> {
    if self.proxy_command == Some(true) || self.proxy_jump == Some(true) {
      return Err(unsupported(
        "The configured SSH proxy cannot be used without starting a connection.",
      ));
    }
    if self.values.contains_key("bindaddress") || self.values.contains_key("bindinterface") {
      return Err(unsupported(
        "SSH configuration selects a source address or interface.",
      ));
    }
    for (keyword, default) in [
      ("addressfamily", "any"),
      ("canonicalizehostname", "no"),
      ("refuseconnection", "no"),
    ] {
      if self
        .values
        .get(keyword)
        .is_some_and(|value| !value.eq_ignore_ascii_case(default))
      {
        return Err(unsupported(
          "SSH configuration requires unsupported endpoint or network selection.",
        ));
      }
    }
    let hostname = self
      .values
      .get("hostname")
      .map_or(self.match_host, String::as_str);
    let host = expand_hostname(hostname, self.match_host)?;
    let port = self.values.get("port").map_or(Ok(22), |port| {
      port
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| unsupported("SSH port is invalid or unsupported."))
    })?;
    Ok(Endpoint { host, port })
  }
}

fn destination_host(destination: &str) -> Result<String, SshReachability> {
  // URI destinations encode additional settings and require a separate parser.
  if destination.contains("://") {
    return Err(unsupported(
      "SSH URI destinations are not supported by the availability check.",
    ));
  }
  let host = destination
    .rsplit_once('@')
    .map_or(destination, |(_, host)| host);
  validate_host(host)
}

fn expand_hostname(value: &str, match_host: &str) -> Result<String, SshReachability> {
  let mut hostname = String::new();
  let mut characters = value.chars();
  while let Some(character) = characters.next() {
    if character != '%' {
      hostname.push(character);
      continue;
    }
    match characters.next() {
      Some('h') => hostname.push_str(match_host),
      Some('%') => hostname.push('%'),
      _ => return Err(unsupported("SSH HostName contains an unsupported token.")),
    }
  }
  validate_host(&hostname)
}

fn validate_host(value: &str) -> Result<String, SshReachability> {
  let host = value
    .strip_prefix('[')
    .and_then(|host| host.strip_suffix(']'))
    .unwrap_or(value);
  if host.is_empty()
    || host.starts_with('-')
    || host.len() > 253
    || host.chars().any(|character| {
      character.is_control()
        || character.is_whitespace()
        || matches!(
          character,
          '/' | '\\' | '@' | '*' | '?' | '[' | ']' | '$' | '#'
        )
    })
  {
    return Err(unsupported("SSH destination is invalid or unsupported."));
  }
  Ok(host.into())
}

fn one_argument(arguments: &[String]) -> Result<&str, SshReachability> {
  match arguments {
    [value] if !value.is_empty() => Ok(value),
    _ => Err(unsupported(
      "SSH configuration contains an invalid argument.",
    )),
  }
}

fn host_matches(host: &str, patterns: &[String]) -> Result<bool, SshReachability> {
  if patterns.is_empty() {
    return Err(unsupported("SSH Host has no pattern."));
  }
  let mut matched = false;
  for pattern in patterns {
    let (negated, pattern) = pattern
      .strip_prefix('!')
      .map_or((false, pattern.as_str()), |pattern| (true, pattern));
    if pattern.is_empty() {
      return Err(unsupported("SSH Host contains an empty pattern."));
    }
    if wildcard_matches(host.as_bytes(), pattern.as_bytes()) {
      if negated {
        return Ok(false);
      }
      matched = true;
    }
  }
  Ok(matched)
}

// OpenSSH Host patterns support only '*' and '?', not glob bracket classes.
fn wildcard_matches(value: &[u8], pattern: &[u8]) -> bool {
  let (mut value_index, mut pattern_index) = (0, 0);
  let (mut star_index, mut retry_index) = (None, 0);
  while value_index < value.len() {
    if pattern
      .get(pattern_index)
      .is_some_and(|character| *character == b'?' || *character == value[value_index])
    {
      value_index += 1;
      pattern_index += 1;
    } else if pattern.get(pattern_index) == Some(&b'*') {
      star_index = Some(pattern_index);
      pattern_index += 1;
      retry_index = value_index;
    } else if let Some(star) = star_index {
      retry_index += 1;
      value_index = retry_index;
      pattern_index = star + 1;
    } else {
      return false;
    }
  }
  pattern[pattern_index..]
    .iter()
    .all(|character| *character == b'*')
}

fn parse_directive(line: &str) -> Result<Option<(String, Vec<String>)>, SshReachability> {
  let line = line.trim();
  if line.is_empty() || line.starts_with('#') {
    return Ok(None);
  }
  let end = line
    .find(|character: char| character.is_ascii_whitespace() || character == '=')
    .unwrap_or(line.len());
  let keyword = line[..end].to_ascii_lowercase();
  let remaining = directive_arguments(line);
  let mut arguments = Vec::new();
  let mut value = String::new();
  let mut quote = None;
  let mut started = false;
  let mut characters = remaining.chars();
  while let Some(character) = characters.next() {
    if character == '\\' {
      let next = characters
        .next()
        .ok_or_else(|| unsupported("SSH configuration has an incomplete escape."))?;
      if !(matches!(next, '\\' | '"' | '\'') || quote.is_none() && next == ' ') {
        return Err(unsupported("SSH configuration uses an unsupported escape."));
      }
      value.push(next);
      started = true;
    } else if let Some(delimiter) = quote {
      if character == delimiter {
        quote = None;
      } else {
        value.push(character);
      }
    } else if character == '"' || character == '\'' {
      quote = Some(character);
      started = true;
    } else if character == '#' && !started {
      break;
    } else if character.is_ascii_whitespace() {
      if started {
        arguments.push(std::mem::take(&mut value));
        started = false;
      }
    } else {
      value.push(character);
      started = true;
    }
  }
  if quote.is_some() {
    return Err(unsupported(
      "SSH configuration contains an unterminated quote.",
    ));
  }
  if started {
    arguments.push(value);
  }
  Ok(Some((keyword, arguments)))
}

fn directive_arguments(line: &str) -> &str {
  let line = line.trim();
  let end = line
    .find(|character: char| character.is_ascii_whitespace() || character == '=')
    .unwrap_or(line.len());
  let arguments = line[end..].trim_start();
  arguments.strip_prefix('=').unwrap_or(arguments).trim()
}

fn is_non_routing_option(keyword: &str) -> bool {
  matches!(
    keyword,
    "addkeystoagent"
      | "batchmode"
      | "canonicaldomains"
      | "canonicalizefallbacklocal"
      | "canonicalizemaxdots"
      | "canonicalizepermittedcnames"
      | "casignaturealgorithms"
      | "certificatefile"
      | "challengeresponseauthentication"
      | "channeltimeout"
      | "checkhostip"
      | "ciphers"
      | "clearallforwardings"
      | "compression"
      | "connectionattempts"
      | "connecttimeout"
      | "controlmaster"
      | "controlpath"
      | "controlpersist"
      | "dynamicforward"
      | "enableescapecommandline"
      | "enablesshkeysign"
      | "escapechar"
      | "exitonforwardfailure"
      | "fingerprinthash"
      | "forkafterauthentication"
      | "forwardagent"
      | "forwardx11"
      | "forwardx11timeout"
      | "forwardx11trusted"
      | "gatewayports"
      | "globalknownhostsfile"
      | "gssapiauthentication"
      | "gssapidelegatecredentials"
      | "hashknownhosts"
      | "hostbasedacceptedalgorithms"
      | "hostbasedacceptedkeytypes"
      | "hostbasedauthentication"
      | "hostkeyalgorithms"
      | "hostkeyalias"
      | "identitiesonly"
      | "identityagent"
      | "identityfile"
      | "ignoreunknown"
      | "ipqos"
      | "kbdinteractiveauthentication"
      | "kbdinteractivedevices"
      | "kexalgorithms"
      | "knownhostscommand"
      | "localcommand"
      | "localforward"
      | "loglevel"
      | "logverbose"
      | "macs"
      | "nohostauthenticationforlocalhost"
      | "numberofpasswordprompts"
      | "obscurekeystroketiming"
      | "passwordauthentication"
      | "permitlocalcommand"
      | "permitremoteopen"
      | "pkcs11provider"
      | "preferredauthentications"
      | "protocol"
      | "proxyusefdpass"
      | "pubkeyacceptedalgorithms"
      | "pubkeyacceptedkeytypes"
      | "pubkeyauthentication"
      | "rekeylimit"
      | "remotecommand"
      | "remoteforward"
      | "requesttty"
      | "requiredrsasize"
      | "revokedhostkeys"
      | "securitykeyprovider"
      | "sendenv"
      | "serveralivecountmax"
      | "serveraliveinterval"
      | "sessiontype"
      | "setenv"
      | "stdinnull"
      | "streamlocalbindmask"
      | "streamlocalbindunlink"
      | "stricthostkeychecking"
      | "syslogfacility"
      | "tag"
      | "tcpkeepalive"
      | "tunnel"
      | "tunneldevice"
      | "updatehostkeys"
      | "usekeychain"
      | "user"
      | "userknownhostsfile"
      | "verifyhostkeydns"
      | "versionaddendum"
      | "visualhostkey"
      | "warnweakcrypto"
      | "xauthlocation"
  )
}

fn unsupported(message: &str) -> SshReachability {
  SshReachability {
    state: SshReachabilityState::NotChecked,
    reason: Some(SshReachabilityReason::UnsupportedConfiguration),
    message: Some(message.into()),
  }
}

fn check_failed() -> SshReachability {
  SshReachability {
    state: SshReachabilityState::Unknown,
    reason: Some(SshReachabilityReason::CheckFailed),
    message: Some("SSH configuration could not be inspected safely.".into()),
  }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
