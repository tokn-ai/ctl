use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

mod write;

pub use write::{SaveSshConfigError, SshHostDefinition, save_host};

const MAX_INCLUDE_DEPTH: usize = 32;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SshHostDiscovery {
  pub hosts: Vec<String>,
  pub warnings: Vec<String>,
  occurrences: HashMap<String, usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeDirectoryUnavailable;

impl fmt::Display for HomeDirectoryUnavailable {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str("could not determine the home directory for SSH configuration")
  }
}

impl std::error::Error for HomeDirectoryUnavailable {}

/// Discovers concrete aliases declared by the user's OpenSSH configuration.
///
/// This is catalog discovery only: it does not resolve or contact any host.
/// Wildcard and negated patterns cannot be selected as destinations, so they
/// are deliberately omitted. Unreadable or malformed includes produce
/// warnings while aliases from the remaining files stay available.
pub fn discover_hosts() -> Result<SshHostDiscovery, HomeDirectoryUnavailable> {
  let home = dirs::home_dir().ok_or(HomeDirectoryUnavailable)?;
  Ok(discover_hosts_from_home(&home))
}

fn discover_hosts_from_home(home: &Path) -> SshHostDiscovery {
  let ssh_directory = home.join(".ssh");
  let mut collector = HostCollector::new(home, &ssh_directory);
  collector.visit(&ssh_directory.join("config"), 0);
  collector.discovery
}

struct HostCollector<'a> {
  home: &'a Path,
  ssh_directory: &'a Path,
  visited_files: HashSet<PathBuf>,
  seen_hosts: HashSet<String>,
  discovery: SshHostDiscovery,
}

impl<'a> HostCollector<'a> {
  fn new(home: &'a Path, ssh_directory: &'a Path) -> Self {
    Self {
      home,
      ssh_directory,
      visited_files: HashSet::new(),
      seen_hosts: HashSet::new(),
      discovery: SshHostDiscovery::default(),
    }
  }

  fn visit(&mut self, path: &Path, depth: usize) {
    if depth > MAX_INCLUDE_DEPTH {
      self.warn(format!(
        "SSH config include depth exceeded at {}",
        path.display()
      ));
      return;
    }

    let canonical_path = match fs::canonicalize(path) {
      Ok(path) => path,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
      Err(error) => {
        self.warn(format!(
          "could not resolve SSH config {}: {error}",
          path.display()
        ));
        return;
      }
    };
    if !self.visited_files.insert(canonical_path.clone()) {
      return;
    }

    let contents = match fs::read_to_string(&canonical_path) {
      Ok(contents) => contents,
      Err(error) => {
        self.warn(format!(
          "could not read SSH config {}: {error}",
          canonical_path.display()
        ));
        return;
      }
    };

    for line in contents.lines() {
      let Some((keyword, arguments)) = parse_directive(line) else {
        continue;
      };
      if keyword.eq_ignore_ascii_case("host") {
        for host in arguments.into_iter().filter(|host| is_concrete_host(host)) {
          *self.discovery.occurrences.entry(host.clone()).or_default() += 1;
          if self.seen_hosts.insert(host.clone()) {
            self.discovery.hosts.push(host);
          }
        }
      } else if keyword.eq_ignore_ascii_case("include") {
        for pattern in arguments {
          for included_path in self.expand_include(&pattern) {
            self.visit(&included_path, depth + 1);
          }
        }
      }
    }
  }

  fn expand_include(&mut self, pattern: &str) -> Vec<PathBuf> {
    let expanded = match expand_include_tokens(pattern, self.home) {
      Ok(expanded) => expanded,
      Err(token) => {
        self.warn(format!(
          "skipped SSH config Include containing context-dependent token %{token}: {pattern}"
        ));
        return Vec::new();
      }
    };
    let path = if expanded == "~" {
      self.home.to_path_buf()
    } else if let Some(relative) = expanded.strip_prefix("~/") {
      self.home.join(relative)
    } else if expanded.starts_with('~') {
      self.warn(format!(
        "skipped SSH config Include with unsupported user-home expansion: {pattern}"
      ));
      return Vec::new();
    } else {
      let path = PathBuf::from(expanded);
      if path.is_absolute() {
        path
      } else {
        self.ssh_directory.join(path)
      }
    };
    let Some(pattern) = path.to_str() else {
      self.warn(format!(
        "skipped non-Unicode SSH config Include path {}",
        path.display()
      ));
      return Vec::new();
    };

    let entries = match glob::glob(pattern) {
      Ok(entries) => entries,
      Err(error) => {
        self.warn(format!(
          "invalid SSH config Include pattern {pattern}: {error}"
        ));
        return Vec::new();
      }
    };
    let mut paths = Vec::new();
    for entry in entries {
      match entry {
        Ok(path) => paths.push(path),
        Err(error) => self.warn(format!("could not inspect SSH config Include: {error}")),
      }
    }
    paths.sort();
    paths
  }

  fn warn(&mut self, warning: String) {
    if !self.discovery.warnings.contains(&warning) {
      self.discovery.warnings.push(warning);
    }
  }
}

fn parse_directive(line: &str) -> Option<(String, Vec<String>)> {
  let line = strip_comment(line).trim();
  if line.is_empty() {
    return None;
  }
  let keyword_end = line
    .find(|character: char| character.is_ascii_whitespace() || character == '=')
    .unwrap_or(line.len());
  let keyword = line[..keyword_end].to_owned();
  let arguments = line[keyword_end..]
    .trim_start_matches(|character: char| character.is_ascii_whitespace() || character == '=');
  Some((keyword, split_arguments(arguments)))
}

fn strip_comment(line: &str) -> &str {
  let mut quote = None;
  let mut escaped = false;
  for (index, character) in line.char_indices() {
    if escaped {
      escaped = false;
      continue;
    }
    if character == '\\' {
      escaped = true;
      continue;
    }
    if let Some(delimiter) = quote {
      if character == delimiter {
        quote = None;
      }
      continue;
    }
    if character == '\'' || character == '"' {
      quote = Some(character);
    } else if character == '#' {
      return &line[..index];
    }
  }
  line
}

fn split_arguments(arguments: &str) -> Vec<String> {
  let mut values = Vec::new();
  let mut value = String::new();
  let mut quote = None;
  let mut escaped = false;
  let mut started = false;

  for character in arguments.chars() {
    if escaped {
      value.push(character);
      escaped = false;
      started = true;
      continue;
    }
    if character == '\\' {
      escaped = true;
      started = true;
      continue;
    }
    if let Some(delimiter) = quote {
      if character == delimiter {
        quote = None;
      } else {
        value.push(character);
      }
      started = true;
      continue;
    }
    if character == '\'' || character == '"' {
      quote = Some(character);
      started = true;
    } else if character.is_ascii_whitespace() {
      if started {
        values.push(std::mem::take(&mut value));
        started = false;
      }
    } else {
      value.push(character);
      started = true;
    }
  }
  if escaped {
    value.push('\\');
  }
  if started {
    values.push(value);
  }
  values
}

fn is_concrete_host(host: &str) -> bool {
  !host.is_empty()
    && !host.starts_with('!')
    && !host.contains(['*', '?'])
    && !host
      .chars()
      .any(|character| character.is_whitespace() || character.is_control())
}

fn expand_include_tokens(pattern: &str, home: &Path) -> Result<String, char> {
  let home = home.to_string_lossy();
  let mut expanded = String::new();
  let mut characters = pattern.chars();
  while let Some(character) = characters.next() {
    if character != '%' {
      expanded.push(character);
      continue;
    }
    let Some(token) = characters.next() else {
      return Err('%');
    };
    match token {
      '%' => expanded.push('%'),
      'd' => expanded.push_str(&home),
      token => return Err(token),
    }
  }
  Ok(expanded)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_concrete_hosts_and_skips_patterns() {
    let lines = [
      "Host workstation lab *.example !production ?ingle",
      "Host=quoted # trailing comment",
      "Host \"another-host\"",
    ];

    let hosts = lines
      .into_iter()
      .filter_map(parse_directive)
      .filter(|(keyword, _)| keyword.eq_ignore_ascii_case("host"))
      .flat_map(|(_, hosts)| hosts)
      .filter(|host| is_concrete_host(host))
      .collect::<Vec<_>>();

    assert_eq!(hosts, ["workstation", "lab", "quoted", "another-host"]);
  }

  #[test]
  fn follows_sorted_includes_once_and_preserves_first_host_order() {
    let temporary = TemporaryDirectory::new();
    let ssh_directory = temporary.path().join(".ssh");
    let config_directory = ssh_directory.join("config.d");
    fs::create_dir_all(&config_directory).unwrap();
    fs::write(
      ssh_directory.join("config"),
      "Host root\nInclude config.d/*.conf\nInclude config\nHost repeated\n",
    )
    .unwrap();
    fs::write(
      config_directory.join("20-second.conf"),
      "Host second repeated\n",
    )
    .unwrap();
    fs::write(config_directory.join("10-first.conf"), "Host first\n").unwrap();

    let discovery = discover_hosts_from_home(temporary.path());

    assert_eq!(discovery.hosts, ["root", "first", "second", "repeated"]);
    assert!(discovery.warnings.is_empty());
  }

  #[test]
  fn expands_home_tokens_and_rejects_host_dependent_include_tokens() {
    let home = Path::new("/Users/tester");

    assert_eq!(
      expand_include_tokens("%d/.ssh/config.d/*.conf", home),
      Ok("/Users/tester/.ssh/config.d/*.conf".into())
    );
    assert_eq!(expand_include_tokens("hosts/%h", home), Err('h'));
  }

  #[test]
  fn keeps_hosts_when_an_include_cannot_be_resolved_for_a_catalog() {
    let temporary = TemporaryDirectory::new();
    let ssh_directory = temporary.path().join(".ssh");
    fs::create_dir_all(&ssh_directory).unwrap();
    fs::write(
      ssh_directory.join("config"),
      "Host before\nInclude hosts/%h\nHost after\n",
    )
    .unwrap();

    let discovery = discover_hosts_from_home(temporary.path());

    assert_eq!(discovery.hosts, ["before", "after"]);
    assert_eq!(discovery.warnings.len(), 1);
    assert!(discovery.warnings[0].contains("context-dependent token %h"));
  }

  struct TemporaryDirectory(PathBuf);

  impl TemporaryDirectory {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("rmux-ssh-config-{}", uuid::Uuid::new_v4()));
      fs::create_dir(&path).unwrap();
      Self(path)
    }

    fn path(&self) -> &Path {
      &self.0
    }
  }

  impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }
}
