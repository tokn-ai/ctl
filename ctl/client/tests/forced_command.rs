#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output};

const MANAGED_PREFIX: &str = concat!(
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  r#"command -v ctl-agent >/dev/null 2>&1 || { printf 'ctl-ssh-nf\n'; exit 127; };"#,
);

struct Fixture {
  directory: PathBuf,
  agent: PathBuf,
  arguments: PathBuf,
  script: String,
}

impl Fixture {
  fn new() -> Self {
    let directory =
      std::env::temp_dir().join(format!("ctl-forced-command-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let agent = directory.join("ctl-agent");
    let arguments = directory.join("arguments");
    fs::write(
      &agent,
      concat!(
        "#!/bin/sh\n",
        "printf '%s\\n' \"$@\" > \"$CTL_TEST_AGENT_ARGUMENTS\"\n",
        "preface=ctl-ssh-v1\n",
        "for argument do\n",
        "  if [ \"$argument\" = --identity ]; then preface=ctl-ssh-v3; fi\n",
        "done\n",
        "printf '%s\\n' \"$preface\"\n",
      ),
    )
    .unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();

    let source =
      PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docker/ctmux/forced-command.sh");
    let source = fs::read_to_string(source).expect("Docker tests require the repository checkout");
    // Replace only the fixed executable, leaving the production allowlist and
    // framing unchanged. Production never accepts an executable override.
    assert!(source.contains("exec /usr/local/bin/ctl-agent \"$@\""));
    let quoted_agent = format!("'{}'", agent.to_str().unwrap().replace('\'', "'\\''"));
    let script = source.replace("/usr/local/bin/ctl-agent", &quoted_agent);
    Self {
      directory,
      agent,
      arguments,
      script,
    }
  }

  fn run(&self, original_command: &str) -> Output {
    Command::new("/bin/sh")
      .args(["-c", &self.script])
      .env_clear()
      .env("SSH_ORIGINAL_COMMAND", original_command)
      .env("CTL_TEST_AGENT_ARGUMENTS", &self.arguments)
      .output()
      .unwrap()
  }

  fn recorded_arguments(&self) -> String {
    fs::read_to_string(&self.arguments).unwrap()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.directory);
  }
}

#[test]
fn authenticated_commands_emit_authentication_before_agent_identity() {
  for service in ["", " --service task"] {
    let fixture = Fixture::new();
    let command = format!(
      "printf 'ctl-ssh-auth-v1\\n'; {MANAGED_PREFIX} exec ctl-agent connect{service} --identity"
    );
    let output = fixture.run(&command);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"ctl-ssh-auth-v1\nctl-ssh-v3\n");
    let expected = if service.is_empty() {
      "connect\n--identity\n"
    } else {
      "connect\n--service\ntask\n--identity\n"
    };
    assert_eq!(fixture.recorded_arguments(), expected);
  }
}

#[test]
fn ordinary_and_identified_commands_keep_their_original_framing() {
  for service in ["", " --service task"] {
    for identity in ["", " --identity"] {
      for prefix in [
        String::new(),
        r#"PATH="$HOME/.tokn/ctl/current:$PATH" "#.to_owned(),
        format!("{MANAGED_PREFIX} "),
      ] {
        let fixture = Fixture::new();
        let command = format!("{prefix}exec ctl-agent connect{service}{identity}");
        let output = fixture.run(&command);
        assert!(output.status.success(), "{output:?}");
        let preface = if identity.is_empty() {
          b"ctl-ssh-v1\n"
        } else {
          b"ctl-ssh-v3\n"
        };
        assert_eq!(output.stdout, preface);
        let expected = format!(
          "connect\n{}{}",
          if service.is_empty() {
            ""
          } else {
            "--service\ntask\n"
          },
          if identity.is_empty() {
            ""
          } else {
            "--identity\n"
          },
        );
        assert_eq!(fixture.recorded_arguments(), expected);
      }
    }
  }
}

#[test]
fn missing_agent_preserves_authenticated_and_plain_missing_markers() {
  for authenticated in [false, true] {
    for missing in [false, true] {
      for service in ["", " --service task"] {
        let fixture = Fixture::new();
        if missing {
          fs::remove_file(&fixture.agent).unwrap();
        } else {
          fs::set_permissions(&fixture.agent, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let prefix = if authenticated {
          "printf 'ctl-ssh-auth-v1\\n'; "
        } else {
          ""
        };
        let command =
          format!("{prefix}{MANAGED_PREFIX} exec ctl-agent connect{service} --identity");
        let output = fixture.run(&command);
        assert_eq!(output.status.code(), Some(127), "{output:?}");
        let expected = if authenticated {
          b"ctl-ssh-auth-v1\nctl-ssh-nf\n".as_slice()
        } else {
          b"ctl-ssh-nf\n".as_slice()
        };
        assert_eq!(output.stdout, expected);
        assert!(!fixture.arguments.exists());
      }
    }
  }
}

#[test]
fn rejected_commands_never_emit_markers_or_execute_the_agent() {
  let commands = [
    "",
    "exec ctl-agent connect; echo injected",
    "exec ctl-agent connect --service task --identity; echo injected",
    "exec ctl-agent connect --service arbitrary",
    "exec ctl-agent connect --socket /tmp/other.sock",
    "exec ctl-agent listeners",
    "ctl-agent connect",
    "printf 'ctl-ssh-auth-v1\\n'; exec ctl-agent connect --identity",
  ]
  .into_iter()
  .map(str::to_owned)
  .chain([format!(
    "printf 'ctl-ssh-auth-v1\\n'; {MANAGED_PREFIX} exec ctl-agent connect --identity; echo injected"
  )]);
  for command in commands {
    let fixture = Fixture::new();
    let output = fixture.run(&command);
    assert_eq!(output.status.code(), Some(126), "{command}: {output:?}");
    assert!(output.stdout.is_empty(), "{command}: {output:?}");
    assert!(!fixture.arguments.exists(), "{command}");
  }
}
