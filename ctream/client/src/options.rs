//! Launcher options shared by the CLI and preview executable.

use crate::transport::{AgentLaunch, Local, SshCommand};
use std::{ffi::OsString, io, path::PathBuf};

#[derive(Default)]
pub struct ConnectionOptions {
  agent: Option<OsString>,
  ssh_program: Option<PathBuf>,
  ssh_arguments: Vec<OsString>,
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidInput, message)
}

impl ConnectionOptions {
  /// Consume a known launcher flag and its value; leave other flags to the app.
  pub fn parse_option(
    &mut self,
    argument: &str,
    arguments: &mut impl Iterator<Item = OsString>,
  ) -> io::Result<bool> {
    if !matches!(argument, "--agent" | "--ssh-program" | "--ssh-arg") {
      return Ok(false);
    }
    let value = arguments
      .next()
      .filter(|value| !value.is_empty())
      .ok_or_else(|| invalid(&format!("{argument} needs a value")))?;
    match argument {
      "--agent" if self.agent.is_none() => self.agent = Some(value),
      "--ssh-program" if self.ssh_program.is_none() => self.ssh_program = Some(value.into()),
      "--ssh-arg" => self.ssh_arguments.push(value),
      _ => return Err(invalid(&format!("duplicate {argument}"))),
    }
    Ok(true)
  }

  pub fn agent_is_explicit(&self) -> bool {
    self.agent.is_some()
  }

  pub fn ssh(&self, host: &str) -> io::Result<SshCommand> {
    if host.is_empty() || host.starts_with('-') {
      return Err(invalid("--host needs an SSH destination"));
    }
    let mut transport = SshCommand::new(host).with_arguments(self.ssh_arguments.clone());
    if let Some(program) = &self.ssh_program {
      transport = transport.with_program(program.clone());
    }
    if let Some(agent) = &self.agent {
      let agent = agent
        .to_str()
        .ok_or_else(|| invalid("remote agent path must be UTF-8"))?;
      transport = transport.with_agent(AgentLaunch::executable(agent)?);
    }
    Ok(transport)
  }

  pub fn local(&self) -> io::Result<Local> {
    if self.ssh_program.is_some() || !self.ssh_arguments.is_empty() {
      return Err(invalid("SSH launcher options require a remote host"));
    }
    Ok(if let Some(agent) = &self.agent {
      Local {
        agent: agent.into(),
      }
    } else {
      Local::default()
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn launch_flags_preserve_values_and_reject_ambiguous_configuration() {
    let mut options = ConnectionOptions::default();
    assert!(
      options
        .parse_option(
          "--agent",
          &mut [OsString::from("/mounted drive/agent.exe")].into_iter()
        )
        .unwrap()
    );
    assert_eq!(
      options.local().unwrap().agent,
      PathBuf::from("/mounted drive/agent.exe")
    );
    assert!(
      options
        .parse_option("--agent", &mut [OsString::from("another")].into_iter())
        .is_err()
    );
    assert!(
      options
        .parse_option("--ssh-arg", &mut [OsString::from("ssh")].into_iter())
        .unwrap()
    );
    assert!(options.local().is_err());
    assert!(options.ssh("-unsafe-option").is_err());
    assert!(
      options
        .parse_option("--ssh-program", &mut std::iter::empty())
        .is_err()
    );
  }
}
