use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const MALFORMED_CATALOG: &[u8] = b"this is not a host catalog; must not be read";

const DOCUMENTS: &[(&str, &str, &[u8])] = &[
  (
    "ctl",
    "SKILL.md",
    include_bytes!("../../../skills/ctl/SKILL.md"),
  ),
  (
    "ctl",
    "references/setup.md",
    include_bytes!("../../../skills/ctl/references/setup.md"),
  ),
  (
    "ctl-host",
    "SKILL.md",
    include_bytes!("../../../skills/ctl-host/SKILL.md"),
  ),
  (
    "ctl-session",
    "SKILL.md",
    include_bytes!("../../../skills/ctl-session/SKILL.md"),
  ),
  (
    "ctl-task",
    "SKILL.md",
    include_bytes!("../../../skills/ctl-task/SKILL.md"),
  ),
  (
    "ctl-task",
    "references/definitions.md",
    include_bytes!("../../../skills/ctl-task/references/definitions.md"),
  ),
  (
    "ctl-port",
    "SKILL.md",
    include_bytes!("../../../skills/ctl-port/SKILL.md"),
  ),
  (
    "ctl-vpn",
    "SKILL.md",
    include_bytes!("../../../skills/ctl-vpn/SKILL.md"),
  ),
  (
    "ctl-vpn",
    "references/setup.md",
    include_bytes!("../../../skills/ctl-vpn/references/setup.md"),
  ),
];

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctl-skill-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    fs::create_dir(path.join("bin")).unwrap();
    fs::create_dir(path.join("work")).unwrap();
    fs::write(path.join("hosts.json"), MALFORMED_CATALOG).unwrap();
    let fixture = Self(path);
    fs::copy(env!("CARGO_BIN_EXE_ctl"), fixture.binary()).unwrap();
    fixture
  }

  fn binary(&self) -> PathBuf {
    self
      .0
      .join("bin")
      .join(if cfg!(windows) { "ctl.exe" } else { "ctl" })
  }

  fn output(&self, args: &[&str]) -> Output {
    Command::new(self.binary())
      .args(args)
      .current_dir(self.0.join("work"))
      .env_remove("CTL_SCP_SSH_TRANSPORT")
      .env("CTL_HOSTS_PATH", self.0.join("hosts.json"))
      .env("CTLD_SOCKET_PATH", self.0.join("ctld.sock"))
      .env("RMUX_RUNTIME_DIR", self.0.join("rmux-runtime"))
      .env("TASKD_RUNTIME_DIR", self.0.join("task-runtime"))
      .env("CTLD_BIN", self.0.join("must-not-start-ctld"))
      .env("RMUXD_BIN", self.0.join("must-not-start-rmuxd"))
      .env("TASKD_BIN", self.0.join("must-not-start-taskd"))
      .output()
      .unwrap()
  }

  fn succeeds(&self, args: &[&str]) -> Vec<u8> {
    let output = self.output(args);
    assert!(
      output.status.success(),
      "{args:?}: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "{args:?}: unexpected stderr");
    output.stdout
  }

  fn fails(&self, args: &[&str]) -> String {
    let output = self.output(args);
    assert!(!output.status.success(), "{args:?}: unexpectedly succeeded");
    assert!(output.stdout.is_empty(), "{args:?}: unexpected stdout");
    String::from_utf8(output.stderr).unwrap()
  }

  fn assert_unchanged(&self) {
    let entries: BTreeSet<_> = fs::read_dir(&self.0)
      .unwrap()
      .map(|entry| entry.unwrap().file_name())
      .collect();
    let expected = ["bin", "hosts.json", "work"].map(std::ffi::OsString::from);
    assert_eq!(entries, BTreeSet::from(expected));
    assert!(fs::read_dir(self.0.join("work")).unwrap().next().is_none());
    assert_eq!(
      fs::read(self.0.join("hosts.json")).unwrap(),
      MALFORMED_CATALOG
    );
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn copied_binary_prints_all_embedded_documents_without_repository_or_services() {
  let fixture = Fixture::new();
  assert_eq!(fixture.succeeds(&["skill"]), DOCUMENTS[0].2);
  for &(name, file, bytes) in DOCUMENTS {
    assert_eq!(fixture.succeeds(&["skill", name, "--file", file]), bytes);
    if file == "SKILL.md" {
      assert_eq!(fixture.succeeds(&["skill", name]), bytes);
    }
  }
  fixture.assert_unchanged();
}

#[test]
fn short_names_select_the_same_child_skills() {
  let fixture = Fixture::new();
  for &(name, file, bytes) in DOCUMENTS {
    if file == "SKILL.md"
      && let Some(alias) = name.strip_prefix("ctl-")
    {
      assert_eq!(fixture.succeeds(&["skill", alias]), bytes);
    }
  }
  assert_eq!(
    fixture.succeeds(&["skill", "vpn", "--file", "references/setup.md"]),
    include_bytes!("../../../skills/ctl-vpn/references/setup.md").as_slice()
  );
  fixture.assert_unchanged();
}

#[test]
fn list_identifies_every_document_with_usable_name_and_file_selectors() {
  let fixture = Fixture::new();
  let output = String::from_utf8(fixture.succeeds(&["skill", "--list"])).unwrap();
  let mut lines = output.lines();
  assert_eq!(lines.next(), Some("NAME\tFILE"));
  let entries: Vec<_> = lines
    .map(|line| line.split_once('\t').expect("each row has a name and file"))
    .collect();
  assert_eq!(entries.len(), DOCUMENTS.len());
  let expected: BTreeSet<_> = DOCUMENTS
    .iter()
    .map(|&(name, file, _)| (name, file))
    .collect();
  assert_eq!(entries.iter().copied().collect::<BTreeSet<_>>(), expected);
  for (name, file) in entries {
    assert_ne!(
      fixture.succeeds(&["skill", name, "--file", file]),
      Vec::<u8>::new()
    );
  }
  fixture.assert_unchanged();
}

#[test]
fn listing_conflicts_with_explicit_document_selection() {
  let fixture = Fixture::new();
  for args in [
    &["skill", "--list", "ctl"][..],
    &["skill", "host", "--list"],
    &["skill", "--list", "--file", "SKILL.md"],
    &["skill", "--file", "references/setup.md", "--list"],
  ] {
    let output = fixture.output(args);
    assert_eq!(output.status.code(), Some(2), "{args:?}");
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--list"));
  }
  fixture.assert_unchanged();
}

#[test]
fn unknown_names_are_reported_by_the_argument_parser() {
  let fixture = Fixture::new();
  for name in ["unknown", "../ctl-host", "ctl-session/SKILL.md"] {
    let output = fixture.output(&["skill", name]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stdout, Vec::<u8>::new());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains(name));
    assert!(error.contains("ctl-host"));
  }
  fixture.assert_unchanged();
}

#[test]
fn file_selection_rejects_missing_documents_and_filesystem_paths() {
  let fixture = Fixture::new();
  for (name, file) in [
    ("ctl", "missing.md"),
    ("ctl-host", "references/setup.md"),
    ("ctl", "references/definitions.md"),
    ("ctl-task", "references/setup.md"),
    ("ctl", "../ctl-host/SKILL.md"),
    ("ctl", "references/../../hosts.json"),
    ("ctl", "./SKILL.md"),
    ("ctl", "/tmp/hosts.json"),
    ("ctl", r"C:\hosts.json"),
    ("ctl", r"references\setup.md"),
  ] {
    let error = fixture.fails(&["skill", name, "--file", file]);
    assert!(error.contains("bundled file"), "{name} {file}: {error}");
    assert!(error.contains(name), "{name} {file}: {error}");
  }
  let path = fixture.0.join("hosts.json");
  fixture.fails(&["skill", "--file", path.to_str().unwrap()]);
  fixture.assert_unchanged();
}

#[test]
fn target_flags_are_rejected_without_loading_the_host_catalog() {
  let fixture = Fixture::new();
  for args in [
    &["-H", "work", "skill"][..],
    &["skill", "--host", "work"],
    &["--method", "VPN", "skill"],
    &["skill", "host", "--method", "VPN"],
    &["-H", "work", "--remote-platform", "windows", "skill"],
    &["skill", "--host", "work", "--remote-platform", "windows"],
  ] {
    let error = fixture.fails(args);
    assert!(
      error.to_ascii_lowercase().contains("skill documentation"),
      "{args:?}: {error}"
    );
    assert!(
      !error.contains("JSON"),
      "{args:?}: read host catalog: {error}"
    );
    assert!(
      !error.contains("expected value"),
      "{args:?}: read host catalog: {error}"
    );
  }
  fixture.assert_unchanged();
}

#[test]
fn help_exposes_skill_discovery_and_document_selection() {
  let fixture = Fixture::new();
  let top_level = String::from_utf8(fixture.succeeds(&["--help"])).unwrap();
  assert!(top_level.contains("skill"));
  let help = String::from_utf8(fixture.succeeds(&["skill", "--help"])).unwrap();
  for option in [
    "--file",
    "--list",
    "ctl-host",
    "ctl-session",
    "ctl-task",
    "ctl-port",
    "ctl-vpn",
  ] {
    assert!(help.contains(option), "skill help is missing {option}");
  }
  fixture.assert_unchanged();
}
