//! Invocation-local OpenSSH authentication. Contains no passwords or saved state.
use std::{ffi::OsString, path::PathBuf, process::Command};

/// The only authentication method permitted on a fresh caller-owned connection.
#[derive(Clone)]
pub enum Credential {
  Key(PathBuf),
  Password {
    program: PathBuf,
    endpoint: PathBuf,
    token: String,
  },
}

/// Resolved endpoint/host-key settings, separate from all SSH authentication config.
#[derive(Clone)]
pub struct Authentication {
  pub credential: Credential,
  pub host_options: Vec<OsString>,
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
      "NumberOfPasswordPrompts=1",
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
    ] {
      command.env_remove(name);
    }
    match &self.credential {
      Credential::Key(path) => {
        command.arg("-i").arg(path);
        for option in [
          "PreferredAuthentications=publickey",
          "PubkeyAuthentication=yes",
          "PasswordAuthentication=no",
          "BatchMode=yes",
        ] {
          command.args(["-o", option]);
        }
      }
      Credential::Password {
        program,
        endpoint,
        token,
      } => {
        for option in [
          "PreferredAuthentications=password",
          "PubkeyAuthentication=no",
          "PasswordAuthentication=yes",
          "IdentityFile=none",
          "BatchMode=no",
        ] {
          command.args(["-o", option]);
        }
        // These values locate/authenticate an owner-only local endpoint; none
        // contains the password. The helper returns it only on its stdout pipe.
        command
          .env("SSH_ASKPASS", program)
          .env("SSH_ASKPASS_REQUIRE", "force")
          .env("DISPLAY", "ctl-caller-auth")
          .env("CTL_CALLER_ASKPASS", "1")
          .env("CTL_CALLER_ENDPOINT", endpoint)
          .env("CTL_CALLER_TOKEN", token);
      }
    }
  }
}
