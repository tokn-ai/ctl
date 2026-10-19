#[path = "support/caller_host_trust.rs"]
mod support;

use serde::Deserialize;
use std::{fs, io::Read as _, path::PathBuf};
use support::{Fixture, INPUT, PASSWORD, Request};

const FINGERPRINT: &str = "SHA256:abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";

#[derive(Clone, Copy)]
struct Case {
  name: &'static str,
  scenario: &'static str,
  reply: Option<&'static str>,
  terminal: bool,
  accepted: bool,
}

const CASES: &[Case] = &[
  Case {
    name: "accept",
    scenario: "unknown",
    reply: Some("yes"),
    terminal: true,
    accepted: true,
  },
  Case {
    name: "decline",
    scenario: "unknown",
    reply: Some("no"),
    terminal: true,
    accepted: false,
  },
  Case {
    name: "trusted",
    scenario: "trusted",
    reply: None,
    terminal: true,
    accepted: true,
  },
  Case {
    name: "trusted-noninteractive",
    scenario: "trusted",
    reply: None,
    terminal: false,
    accepted: true,
  },
  Case {
    name: "changed",
    scenario: "changed",
    reply: None,
    terminal: true,
    accepted: false,
  },
  Case {
    name: "strict",
    scenario: "strict",
    reply: None,
    terminal: true,
    accepted: false,
  },
  Case {
    name: "noninteractive",
    scenario: "unknown",
    reply: None,
    terminal: false,
    accepted: false,
  },
];

#[test]
fn caller_host_trust_child_launcher() {
  support::child_launcher();
}

fn arguments(destination: &str, key: Option<&std::path::Path>) -> Vec<String> {
  let mut arguments = vec!["--destination".into(), destination.into()];
  if let Some(key) = key {
    arguments.extend(["--identity-file".into(), key.to_str().unwrap().into()]);
  }
  arguments
}

#[test]
fn caller_credentials_require_host_trust_before_authentication_and_preserve_command_stdin() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new(true);
  for mode in ["key", "password"] {
    let encrypted = Case {
      name: "encrypted",
      scenario: "encrypted",
      reply: None,
      terminal: true,
      accepted: false,
    };
    for case in CASES.iter().copied().chain(std::iter::once(encrypted)) {
      let name = format!("{mode}-{}", case.name);
      let trust = fixture.root.join(format!("{name}.known-hosts"));
      let events = fixture.root.join(format!("{name}.events"));
      let previous = match case.scenario {
        "trusted" | "encrypted" => Some("fixture-known-key\n"),
        "changed" => Some("previous-host-key\n"),
        _ => None,
      };
      if let Some(previous) = previous {
        fs::write(&trust, previous).unwrap();
      }
      let mut arguments = arguments(
        "fixture",
        (mode == "key").then_some(fixture.root.join("key").as_path()),
      );
      arguments.extend(["exec".into(), "--".into(), "cat".into()]);
      let result = fixture.run(Request {
        arguments,
        environment: vec![
          ("CTL_TEST_TRUST_CASE".into(), case.scenario.into()),
          ("CTL_TEST_TRUST_FILE".into(), trust.clone().into_os_string()),
          (
            "CTL_TEST_TRUST_EVENTS".into(),
            events.clone().into_os_string(),
          ),
        ],
        password: (mode == "password").then_some(PASSWORD),
        reply: case.reply,
        terminal: case.terminal,
        input: INPUT,
      });
      let events = fs::read_to_string(events).unwrap();
      if case.accepted {
        assert_eq!(result.code, 17, "{name}: {}", result.transcript);
        assert_eq!(result.stdout, INPUT, "{name}: trust consumed command stdin");
        let trust_event = if case.scenario == "unknown" {
          "trust-approved"
        } else {
          "trust-known"
        };
        assert_eq!(
          events,
          format!("trust-check\n{trust_event}\nauthenticate-{mode}\ncommand\n"),
          "{name}"
        );
      } else {
        assert_eq!(result.code, 255, "{name}: {}", result.transcript);
        assert!(
          result.stdout.is_empty(),
          "{name}: command ran before trust/auth approval"
        );
        assert!(
          !events.contains("authenticate-"),
          "{name}: credentials used before approval"
        );
        assert!(
          !events.contains("command"),
          "{name}: command ran before approval"
        );
      }
      if case.reply.is_some() {
        assert!(
          result.transcript.contains(FINGERPRINT),
          "{name}: fingerprint was not shown"
        );
      }
      let actual = fs::read_to_string(&trust).ok();
      let expected = if case.name == "accept" {
        Some("fixture-known-key\n")
      } else {
        previous
      };
      assert_eq!(
        actual.as_deref(),
        expected,
        "{name}: unexpected trust-file mutation"
      );
      if case.name == "noninteractive" {
        assert!(
          result.transcript.to_ascii_lowercase().contains("terminal"),
          "{name}: {}",
          result.transcript
        );
      }
    }
  }
  assert!(!fixture.root.join("must-not-start.sock").exists());
}

#[derive(Deserialize)]
struct NativeFixture {
  key_user: String,
  key_file: PathBuf,
  server_key: String,
  different_key: String,
  fingerprint: String,
  port: u16,
  include_file: PathBuf,
  #[serde(default)]
  password_user: Option<String>,
  #[serde(default)]
  password: Option<String>,
}

fn native_config(
  metadata: &NativeFixture,
  alias: &str,
  trust: &std::path::Path,
  policy: &str,
) -> String {
  let trust = trust.to_string_lossy().replace('\\', "/");
  format!(
    "Host {alias}\n  HostName 127.0.0.1\n  User {}\n  Port {}\n  HostKeyAlias {alias}\n  UserKnownHostsFile \"{trust}\"\n  GlobalKnownHostsFile none\n  StrictHostKeyChecking {policy}\n  BatchMode yes\n  IdentitiesOnly yes\n  ControlMaster auto\n",
    metadata.key_user, metadata.port,
  )
}

fn native_arguments(metadata: &NativeFixture, alias: &str, mode: &str) -> Vec<String> {
  let mut arguments = arguments(
    alias,
    (mode == "key").then_some(metadata.key_file.as_path()),
  );
  let user = if mode == "password" {
    metadata
      .password_user
      .as_ref()
      .expect("native password user")
  } else {
    &metadata.key_user
  };
  arguments.extend(["--user".into(), user.clone()]);
  #[cfg(windows)]
  arguments.extend([
    "--remote-platform".into(),
    "windows".into(),
    "exec".into(),
    "--".into(),
    "cmd.exe".into(),
    "/D".into(),
    "/C".into(),
    "findstr.exe CALLER_STDIN & exit /b 23".into(),
  ]);
  #[cfg(unix)]
  arguments.extend([
    "exec".into(),
    "--".into(),
    "sh".into(),
    "-c".into(),
    "cat; exit 23".into(),
  ]);
  arguments
}

#[test]
#[ignore = "requires the isolated real OpenSSH fixture; invoked on Unix and Windows in CI"]
fn native_openssh_requires_host_trust_with_redirected_command_stdin() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire_blocking();
  let mut input = String::new();
  std::io::stdin().read_to_string(&mut input).unwrap();
  let metadata: NativeFixture =
    serde_json::from_str(&input).expect("native fixture metadata on stdin");
  let fixture = Fixture::new(false);
  let modes = if metadata.password.is_some() {
    vec!["key", "password"]
  } else {
    vec!["key"]
  };
  for mode in modes {
    for case in CASES {
      let alias = format!("ctl-first-use-{mode}-{}", case.name);
      let trust = fixture.root.join(format!("{alias}.known-hosts"));
      let previous = match case.scenario {
        "trusted" => Some(format!("{alias} {}\n", metadata.server_key)),
        "changed" => Some(format!("{alias} {}\n", metadata.different_key)),
        _ => None,
      };
      if let Some(previous) = &previous {
        fs::write(&trust, previous).unwrap();
      }
      let policy = if case.scenario == "strict" {
        "yes"
      } else {
        "ask"
      };
      fs::write(
        &metadata.include_file,
        native_config(&metadata, &alias, &trust, policy),
      )
      .unwrap();
      #[cfg(windows)]
      let command_input = b"CALLER_STDIN\r\n".as_slice();
      #[cfg(unix)]
      let command_input = b"CALLER_STDIN\n".as_slice();
      let result = fixture.run(Request {
        arguments: native_arguments(&metadata, &alias, mode),
        environment: Vec::new(),
        password: (mode == "password").then(|| metadata.password.as_deref().unwrap()),
        reply: case.reply,
        terminal: case.terminal,
        input: command_input,
      });
      if case.accepted {
        assert_eq!(result.code, 23, "{alias}: {}", result.transcript);
        assert_eq!(
          result.stdout, command_input,
          "{alias}: redirected stdin changed"
        );
      } else {
        assert_eq!(result.code, 255, "{alias}: {}", result.transcript);
        assert!(
          result.stdout.is_empty(),
          "{alias}: rejected connection ran a command"
        );
      }
      if case.reply.is_some() {
        assert!(
          result.transcript.contains(&metadata.fingerprint),
          "{alias}: server fingerprint absent"
        );
      }
      let actual = fs::read_to_string(&trust).ok();
      if case.name == "accept" {
        assert!(
          actual
            .as_deref()
            .is_some_and(|text| text.contains(&metadata.server_key)),
          "{alias}: approved server key was not persisted"
        );
      } else {
        assert_eq!(
          actual, previous,
          "{alias}: trust-file contents changed unexpectedly"
        );
      }
      if case.name == "changed" {
        assert!(
          result
            .transcript
            .contains("REMOTE HOST IDENTIFICATION HAS CHANGED"),
          "{alias}: {}",
          result.transcript
        );
      }
    }
  }
  fs::write(metadata.include_file, "").unwrap();
}
