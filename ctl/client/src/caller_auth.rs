//! Invocation-local OpenSSH authentication. Contains no passwords or saved state.
use std::{ffi::OsString, path::PathBuf, process::Command};

/// The only authentication method permitted on a fresh caller-owned connection.
#[derive(Clone)]
pub enum Credential {
  Key(PathBuf),
  Password { endpoint: PathBuf, token: String },
}

/// Resolved endpoint/host-key settings, separate from all SSH authentication config.
#[derive(Clone)]
pub struct Authentication {
  pub credential: Credential,
  pub host_options: Vec<OsString>,
  /// Invocation-local helper that separates host trust from credential prompts.
  pub prompt_program: PathBuf,
  /// Whether the caller had a controlling terminal before spawning OpenSSH.
  pub allow_host_key_confirmation: bool,
}

impl Authentication {
  /// Configures a fresh, non-detaching connection with exactly one credential.
  /// No existing master, identity agent, configuration identity or saved password
  /// can substitute for the supplied credential. Host-key policy is retained.
  pub fn configure(&self, command: &mut Command) {
    command.args(["-F", "none"]);
    for option in [
      "ControlMaster=no",
      "ControlPath=none",
      "ControlPersist=no",
      "IdentityAgent=none",
      "IdentitiesOnly=yes",
      "AddKeysToAgent=no",
      "CertificateFile=none",
      "KbdInteractiveAuthentication=no",
      "HostbasedAuthentication=no",
      "GSSAPIAuthentication=no",
      "ForwardAgent=no",
      "ClearAllForwardings=yes",
      "PermitLocalCommand=no",
      "RemoteCommand=none",
      "ForkAfterAuthentication=no",
      "StdinNull=no",
      "BatchMode=no",
    ] {
      command.args(["-o", option]);
    }
    command.args(&self.host_options);
    for name in [
      "SSH_ASKPASS",
      "SSH_ASKPASS_REQUIRE",
      "SSH_ASKPASS_PROMPT",
      "CTL_CALLER_ASKPASS",
      "CTL_CALLER_ENDPOINT",
      "CTL_CALLER_TOKEN",
      "CTL_CALLER_CONFIRM_TTY",
    ] {
      command.env_remove(name);
    }
    // BatchMode would suppress host-key confirmation too. Route every prompt
    // through our helper so only host trust can use the controlling terminal;
    // passphrase/PIN prompts cannot fall back to OpenSSH's terminal reader.
    command
      .env("SSH_ASKPASS", &self.prompt_program)
      .env("SSH_ASKPASS_REQUIRE", "force")
      .env("DISPLAY", "ctl-caller-auth")
      .env("CTL_CALLER_ASKPASS", "1")
      .env(
        "CTL_CALLER_CONFIRM_TTY",
        if self.allow_host_key_confirmation {
          "1"
        } else {
          "0"
        },
      );
    match &self.credential {
      Credential::Key(path) => {
        command.arg("-i").arg(path);
        for option in [
          "PreferredAuthentications=publickey",
          "PubkeyAuthentication=yes",
          "PasswordAuthentication=no",
          "NumberOfPasswordPrompts=0",
        ] {
          command.args(["-o", option]);
        }
      }
      Credential::Password { endpoint, token } => {
        for option in [
          "PreferredAuthentications=password",
          "PubkeyAuthentication=no",
          "PasswordAuthentication=yes",
          "IdentityFile=none",
          "NumberOfPasswordPrompts=1",
        ] {
          command.args(["-o", option]);
        }
        // These values locate/authenticate an owner-only local endpoint; none
        // contains the password. The helper returns it only on its stdout pipe.
        command
          .env("CTL_CALLER_ENDPOINT", endpoint)
          .env("CTL_CALLER_TOKEN", token);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::ffi::OsStr;

  fn configure(credential: Credential, allow_host_key_confirmation: bool) -> Command {
    let authentication = Authentication {
      credential,
      host_options: vec!["-o".into(), "StrictHostKeyChecking=yes".into()],
      prompt_program: "caller-helper".into(),
      allow_host_key_confirmation,
    };
    let mut command = Command::new("ssh");
    for name in [
      "SSH_ASKPASS",
      "SSH_ASKPASS_REQUIRE",
      "SSH_ASKPASS_PROMPT",
      "CTL_CALLER_ASKPASS",
      "CTL_CALLER_ENDPOINT",
      "CTL_CALLER_TOKEN",
      "CTL_CALLER_CONFIRM_TTY",
    ] {
      command.env(name, "inherited-value");
    }
    authentication.configure(&mut command);
    command
  }

  fn assert_environment(command: &Command, name: &str, expected: Option<&str>) {
    let value = command
      .get_envs()
      .find_map(|(key, value)| (key == name).then_some(value))
      .expect("authentication must explicitly configure its environment");
    assert_eq!(value, expected.map(OsStr::new), "{name}");
  }

  fn assert_option(command: &Command, option: &str, value: &str) {
    let prefix = format!("{option}=");
    let values: Vec<_> = command
      .get_args()
      .filter_map(|arg| arg.to_str()?.strip_prefix(&prefix))
      .collect();
    // OpenSSH uses the first option. Duplicated mode-specific options can
    // silently restore a passphrase prompt or suppress host-key confirmation.
    assert_eq!(values, [value], "{option}");
  }

  fn assert_common_configuration(command: &Command) {
    assert_environment(command, "SSH_ASKPASS", Some("caller-helper"));
    assert_environment(command, "SSH_ASKPASS_REQUIRE", Some("force"));
    assert_environment(command, "SSH_ASKPASS_PROMPT", None);
    assert_environment(command, "CTL_CALLER_ASKPASS", Some("1"));
    assert_environment(command, "CTL_CALLER_CONFIRM_TTY", Some("1"));
    assert_option(command, "BatchMode", "no");
    assert_option(command, "StrictHostKeyChecking", "yes");
    assert_option(command, "IdentityAgent", "none");
    assert_option(command, "ControlPath", "none");
    assert_option(command, "KbdInteractiveAuthentication", "no");
  }

  #[test]
  fn key_allows_host_trust_but_disables_secret_prompts_and_inherited_password_bridge() {
    let command = configure(Credential::Key("supplied-private-key".into()), true);
    assert_common_configuration(&command);
    assert_option(&command, "NumberOfPasswordPrompts", "0");
    assert_option(&command, "PreferredAuthentications", "publickey");
    assert_option(&command, "PasswordAuthentication", "no");
    assert_environment(&command, "CTL_CALLER_ENDPOINT", None);
    assert_environment(&command, "CTL_CALLER_TOKEN", None);
    let args: Vec<_> = command.get_args().collect();
    assert!(
      args
        .windows(2)
        .any(|pair| pair == ["-i", "supplied-private-key"])
    );
  }

  #[test]
  fn password_preserves_one_supplied_attempt_with_a_separate_trust_prompt() {
    let command = configure(
      Credential::Password {
        endpoint: "invocation-endpoint".into(),
        token: "invocation-token".into(),
      },
      true,
    );
    assert_common_configuration(&command);
    assert_option(&command, "NumberOfPasswordPrompts", "1");
    assert_option(&command, "PreferredAuthentications", "password");
    assert_option(&command, "PubkeyAuthentication", "no");
    assert_option(&command, "IdentityFile", "none");
    assert_environment(&command, "CTL_CALLER_ENDPOINT", Some("invocation-endpoint"));
    assert_environment(&command, "CTL_CALLER_TOKEN", Some("invocation-token"));
    assert!(!command.get_args().any(|arg| arg == "-i"));
  }

  #[test]
  fn absent_caller_terminal_cannot_inherit_confirmation_permission() {
    for credential in [
      Credential::Key("supplied-private-key".into()),
      Credential::Password {
        endpoint: "invocation-endpoint".into(),
        token: "invocation-token".into(),
      },
    ] {
      let command = configure(credential, false);
      assert_environment(&command, "CTL_CALLER_CONFIRM_TTY", Some("0"));
      assert_environment(&command, "SSH_ASKPASS_REQUIRE", Some("force"));
    }
  }
}
