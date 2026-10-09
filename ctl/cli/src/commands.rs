use super::{Arguments, Command, RemotePlatform};
use ctl_client::{
  ConnectionTarget, CoreError, TaskTransport, Transport, open_task_transport_with_interaction,
  open_transport_with_interaction,
};
use ctmux_cli::{CommandError, ConnectFuture, Connector};
use std::path::PathBuf;
use std::sync::{
  Arc,
  atomic::{AtomicBool, Ordering},
};
use thiserror::Error;

mod reconnect;
#[cfg(all(test, unix, ctl_repository_tui_tests))]
mod tui_tests;

fn validate_local_command_target(arguments: &Arguments) -> Result<(), CliError> {
  let error = match &arguments.command {
    #[cfg(unix)]
    Command::Components {
      command:
        crate::components::Command::Update { .. }
        | crate::components::Command::Status { .. }
        | crate::components::Command::Restart { .. },
    } => return Ok(()),
    #[cfg(unix)]
    Command::Components { .. } => CliError::ComponentsTarget,
    Command::Setup(_) => CliError::SetupTarget,
    Command::Skill(_) => CliError::SkillTarget,
    Command::Passwords { .. } => CliError::PasswordsTarget,
    Command::Logs(_) | Command::Audit(_) => CliError::HistoryTarget,
    _ => return Ok(()),
  };
  if arguments.host.is_some() || arguments.method.is_some() || arguments.remote_platform.is_some() {
    Err(error)
  } else {
    Ok(())
  }
}

pub async fn run(arguments: Arguments) -> Result<i32, CliError> {
  validate_local_command_target(&arguments)?;
  if arguments.host.is_some() {
    if matches!(arguments.command, Command::Taskd { .. }) {
      return Err(CliError::RemoteTaskDaemonRestartUnsupported);
    }
    if matches!(arguments.command, Command::Ssh { .. } | Command::Scp { .. }) {
      return Err(CliError::CompatibilityHost);
    }
  }
  if matches!(
    arguments.command,
    Command::Vpn {
      command: crate::vpn::Command::Create { .. } | crate::vpn::Command::Remove { .. }
    }
  ) && (arguments.host.is_some() || arguments.method.is_some())
  {
    return Err(crate::vpn::Error::LocalProfilesOnly.into());
  }
  if matches!(arguments.command, Command::Vpn { .. })
    && matches!(arguments.remote_platform, Some(RemotePlatform::Windows))
  {
    return Err(CliError::RemoteVpnUnsupported);
  }
  if let Some(result) = crate::history::dispatch(&arguments.command) {
    result.map_err(CliError::History)?;
    return Ok(0);
  }
  match arguments.command {
    #[cfg(unix)]
    Command::Components { command } => {
      crate::components::run(
        command,
        arguments.host.as_deref(),
        arguments.method.as_deref(),
        arguments.remote_platform,
      )
      .await
      .map_err(CliError::Components)?;
      return Ok(0);
    }
    Command::Setup(setup_arguments) => {
      crate::setup::run(setup_arguments).await?;
      return Ok(0);
    }
    Command::Skill(skill_arguments) => {
      crate::skill::run(skill_arguments)?;
      return Ok(0);
    }
    Command::Host { command } => {
      if arguments.host.is_some() || arguments.remote_platform.is_some() {
        return Err(CliError::HostManagementTarget);
      }
      crate::host::run(command, arguments.method.as_deref()).await?;
      return Ok(0);
    }
    Command::Passwords { command, json } => {
      crate::passwords::run(command, json).await?;
      return Ok(0);
    }
    Command::Ssh {
      arguments: ssh_arguments,
    } => return Ok(crate::openssh::run_ssh(ssh_arguments, arguments.method.as_deref()).await?),
    Command::Scp {
      arguments: scp_arguments,
    } => {
      return Ok(crate::openssh::run_scp(
        scp_arguments,
        arguments.method.as_deref(),
      )?);
    }
    _ => {}
  }
  let resolved =
    crate::target::resolve(arguments.host.as_deref(), arguments.method.as_deref()).await?;
  let platform = match arguments.remote_platform {
    Some(RemotePlatform::Windows) => ctl_client::RemotePlatform::Windows,
    _ => ctl_client::RemotePlatform::Unix,
  };
  let mut target = resolved.target.to_core();
  if let ConnectionTarget::Ssh { options, .. } = &mut target {
    options.remote_platform = platform;
  }
  let connector = CtlConnector {
    target,
    settings: resolved.target,
    recovery: Arc::default(),
    terminal_ui_active: Arc::default(),
  };
  let operation = run_selected(arguments.command, &connector, platform);
  tokio::select! {
    result = operation => result,
    interrupted = connector.recovery.interrupt() => {
      interrupted.map_err(CliError::Interrupt)?;
      Err(CliError::RepairCancelled)
    }
  }
}

async fn run_selected(
  command: Command,
  connector: &CtlConnector,
  platform: ctl_client::RemotePlatform,
) -> Result<i32, CliError> {
  match command {
    Command::Shell {
      session,
      plain,
      cwd,
    } => return run_shell(connector, session, plain, cwd).await,
    Command::Exec { command } => {
      return Ok(crate::connection::execute(&connector.settings, command, platform).await?);
    }
    #[cfg(unix)]
    Command::Port { command } => crate::port::run(&connector.settings, command).await?,
    Command::Logs(_)
    | Command::Audit(_)
    | Command::Setup(_)
    | Command::Skill(_)
    | Command::Host { .. }
    | Command::Passwords { .. }
    | Command::Ssh { .. }
    | Command::Scp { .. } => {
      unreachable!("commands dispatched before target resolution")
    }
    #[cfg(unix)]
    Command::Components { .. } => {
      unreachable!("component store commands run before target resolution")
    }
    Command::Ctmux { command } => {
      ctmux_cli::run(command, connector).await?;
    }
    Command::Taskd {
      command: super::TaskdCommand::Restart,
    } => {
      if !connector.target.is_local() {
        return Err(CliError::RemoteTaskDaemonRestartUnsupported);
      }
      ctl_task_client::restart_daemon().await?;
      println!("ctl-taskd is ready");
    }
    Command::Task { command } => {
      ctl_task_cli::run_with_connector(command, connector).await?;
    }
    Command::Vpn { command } => {
      crate::vpn::run(command, &connector.settings).await?;
    }
  }
  Ok(0)
}

async fn run_shell(
  connector: &CtlConnector,
  session: Option<String>,
  plain: bool,
  cwd: Option<String>,
) -> Result<i32, CliError> {
  use std::io::IsTerminal as _;
  if plain {
    return Ok(crate::connection::plain_shell(&connector.settings).await?);
  }
  if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
    return Err(CliError::TerminalRequired);
  }
  let attach_if_exists = session.is_some();
  let session =
    ctmux_cli::new_session(connector, session, Vec::new(), cwd, attach_if_exists).await?;
  ctmux_cli::run_tui(connector, Some(session.session_id), false).await?;
  Ok(0)
}

struct CtlConnector {
  target: ConnectionTarget,
  settings: ctl_client::hosts::ConnectionTargetDto,
  recovery: Arc<crate::remote::Recovery>,
  terminal_ui_active: Arc<AtomicBool>,
}

impl CtlConnector {
  async fn identified_remote(
    &self,
    interaction: &ctl_client::SshInteraction,
    service: ctl_client::RemoteService,
  ) -> Result<Option<ctl_client::SshTransport>, CtlConnectError> {
    let ConnectionTarget::Ssh {
      destination,
      options,
    } = &self.target
    else {
      return Ok(None);
    };
    // Repair may be offered only during the initial connection. Reconnects and
    // interactive task attachments share the same state and never prompt again.
    let stream = self
      .recovery
      .connect(
        || ctl_client::open_identified_ssh_service(destination, options, interaction, service),
        |error| async move {
          if crate::remote::offer_repair(
            &error,
            destination,
            options,
            interaction,
            service,
            &self.settings,
            &self.recovery,
          )
          .await?
          {
            Ok(())
          } else {
            Err(CtlConnectError::from(error))
          }
        },
      )
      .await?;
    let identity = stream.remote_identity.as_ref().ok_or_else(|| {
      ctl_client::hosts::HostError::new(
        "identity_missing",
        "The remote host did not provide its saved identity.",
      )
    })?;
    self.settings.verify_remote_identity(identity)?;
    Ok(Some(stream))
  }

  fn for_interactive_session(&self, ctmux_socket: PathBuf) -> Self {
    Self {
      settings: self.settings.clone(),
      recovery: Arc::clone(&self.recovery),
      terminal_ui_active: Arc::clone(&self.terminal_ui_active),
      target: match &self.target {
        ConnectionTarget::Local { .. } => ConnectionTarget::Local {
          socket_path: ctmux_socket,
        },
        ConnectionTarget::Ssh { .. } => self.target.clone(),
      },
    }
  }
}

impl ctl_task_cli::Connector for CtlConnector {
  type Stream = TaskTransport;
  type Error = CtlConnectError;

  fn connect_task(&self) -> ctl_task_cli::ConnectFuture<'_, TaskTransport, CtlConnectError> {
    Box::pin(async {
      let interaction = ssh_interaction(
        &self.settings,
        !self.terminal_ui_active.load(Ordering::Acquire),
      )
      .await?;
      if let Some(stream) = self
        .identified_remote(&interaction, ctl_client::RemoteService::Task)
        .await?
      {
        return Ok(Transport::Ssh(stream));
      }
      open_task_transport_with_interaction(&self.target, &interaction)
        .await
        .map_err(Into::into)
    })
  }

  fn is_local_task_target(&self) -> bool {
    self.target.is_local()
  }

  fn attach_interactive(
    &self,
    session: String,
    ctmux_socket: PathBuf,
  ) -> ctl_task_cli::AttachFuture<'_> {
    let connector = self.for_interactive_session(ctmux_socket);
    Box::pin(async move { ctl_task_cli::attach_session(session, &connector).await })
  }
}

impl Connector for CtlConnector {
  type Stream = Transport;
  type Error = CtlConnectError;

  fn connect(&self) -> ConnectFuture<'_, Transport, CtlConnectError> {
    Box::pin(async {
      let interaction = ssh_interaction(
        &self.settings,
        !self.terminal_ui_active.load(Ordering::Acquire),
      )
      .await?;
      if let Some(stream) = self
        .identified_remote(&interaction, ctl_client::RemoteService::Ctmux)
        .await?
      {
        return Ok(Transport::Ssh(stream));
      }
      open_transport_with_interaction(&self.target, &interaction)
        .await
        .map_err(Into::into)
    })
  }

  fn is_retryable(&self, error: &CtlConnectError) -> bool {
    reconnect::is_retryable(error)
  }

  fn set_terminal_ui_active(&self, active: bool) {
    self.terminal_ui_active.store(active, Ordering::Release);
  }

  fn is_local(&self) -> bool {
    self.target.is_local()
  }

  fn label(&self) -> &str {
    self.target.label()
  }

  fn connection_kind(&self) -> &'static str {
    if self.target.is_local() {
      "local"
    } else {
      "SSH"
    }
  }

  fn client_name(&self) -> &'static str {
    "ctl"
  }

  fn status_prefix(&self) -> &'static str {
    "ctl"
  }
}

#[cfg(unix)]
async fn ssh_interaction(
  target: &ctl_client::hosts::ConnectionTargetDto,
  interactive: bool,
) -> Result<ctl_client::SshInteraction, CtlConnectError> {
  if target.is_local() {
    return Ok(ctl_client::SshInteraction::Inherit);
  }
  crate::target::ensure_vpn_with_interaction(target, interactive).await?;
  let control_path =
    crate::ssh_broker::ensure_master_with_interaction(target.to_ssh_target()?, interactive).await?;
  Ok(ctl_client::SshInteraction::Multiplexed { control_path })
}

#[cfg(not(unix))]
async fn ssh_interaction(
  _target: &ctl_client::hosts::ConnectionTargetDto,
  interactive: bool,
) -> Result<ctl_client::SshInteraction, CtlConnectError> {
  Ok(if interactive {
    ctl_client::SshInteraction::Inherit
  } else {
    ctl_client::SshInteraction::Batch
  })
}

#[derive(Debug, Error)]
enum CtlConnectError {
  #[error(transparent)]
  Repair(#[from] crate::remote::Error),
  #[error(transparent)]
  Host(#[from] ctl_client::hosts::HostError),
  #[error(transparent)]
  Target(#[from] crate::target::Error),
  #[error(transparent)]
  Core(#[from] CoreError),
  #[cfg(unix)]
  #[error(transparent)]
  Broker(#[from] crate::ssh_broker::Error),
}

#[derive(Debug, Error)]
pub enum CliError {
  #[error("History is local; omit --host, --method, and --remote-platform.")]
  HistoryTarget,
  #[error("Could not read local history: {0}")]
  History(std::io::Error),
  #[cfg(unix)]
  #[error(
    "Components manage the local bundle store; omit --host, --method, and --remote-platform."
  )]
  ComponentsTarget,
  #[cfg(unix)]
  #[error("Component maintenance failed: {0}")]
  Components(std::io::Error),
  #[error("Remote repair cancelled.")]
  RepairCancelled,
  #[error("Could not listen for cancellation: {0}")]
  Interrupt(std::io::Error),
  #[error(transparent)]
  Setup(#[from] ctl_client::setup::Error),
  #[error("Setup installs the local signed helper; omit --host, --method, and --remote-platform.")]
  SetupTarget,
  #[error(transparent)]
  Skill(#[from] crate::skill::Error),
  #[error("Skill documentation is bundled locally; omit --host, --method, and --remote-platform.")]
  SkillTarget,
  #[error(
    "Passwords manage the local credential store; omit --host, --method, and --remote-platform."
  )]
  PasswordsTarget,
  #[error(transparent)]
  Passwords(#[from] crate::passwords::Error),
  #[error(transparent)]
  HostCommand(#[from] crate::host::Error),
  #[error(
    "Host management edits the local catalog; select a host with its positional name or ID, not --host."
  )]
  HostManagementTarget,
  #[error(transparent)]
  Host(#[from] ctl_client::hosts::HostError),
  #[error(transparent)]
  Connection(#[from] crate::connection::Error),
  #[cfg(unix)]
  #[error(transparent)]
  Port(#[from] crate::port::Error),
  #[error("Use the destination argument with ssh/scp, rather than --host.")]
  CompatibilityHost,
  #[error("ctl shell requires a terminal; use ctl exec for commands or ctl ctmux new --detached.")]
  TerminalRequired,
  #[error("Remote VPN execution currently requires a Unix host.")]
  RemoteVpnUnsupported,
  #[error(transparent)]
  Vpn(#[from] crate::vpn::Error),
  #[error("ctl-taskd restart is only supported locally; run it on the task host")]
  RemoteTaskDaemonRestartUnsupported,
  #[error(transparent)]
  Ctmux(#[from] CommandError),
  #[error(transparent)]
  TaskDaemon(#[from] ctl_task_client::ClientError),
  #[error(transparent)]
  Task(#[from] ctl_task_cli::CommandError),
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(unix)]
  #[test]
  fn component_updates_accept_batch_targets_and_only_supported_package_choices() {
    use clap::Parser;
    let arguments = Arguments::try_parse_from([
      "ctl",
      "--host",
      "work",
      "--method",
      "vpn",
      "components",
      "update",
      "--hosts",
      "jump-a,jump-b",
      "--local",
      "--package",
      "ctl-agent",
      "--from",
      "build",
      "--local-build",
      "--ctld-package",
      "package",
      "--json",
    ])
    .unwrap();
    assert!(validate_local_command_target(&arguments).is_ok());
    let Command::Components {
      command:
        crate::components::Command::Update {
          hosts,
          local,
          package,
          from,
          local_build,
          ctld_package,
          json,
        },
    } = arguments.command
    else {
      panic!("expected a component update")
    };
    assert_eq!(hosts, ["jump-a", "jump-b"]);
    assert!(local && local_build && json);
    assert!(matches!(
      package,
      crate::components::UpdatePackage::CtlAgent
    ));
    assert_eq!(from, Some(PathBuf::from("build")));
    assert_eq!(ctld_package, Some(PathBuf::from("package")));
    for flags in [
      vec!["--package", "ctmuxd"],
      vec!["--local-build"],
      vec!["--from", "build", "--ctld-package", "package"],
    ] {
      assert!(
        Arguments::try_parse_from([vec!["ctl", "components", "update"], flags].concat()).is_err()
      );
    }
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn component_store_commands_reject_connection_targets_before_io() {
    use clap::Parser;
    for flags in [
      &["--host", "remote"][..],
      &["--method", "route"][..],
      &["--host", "remote", "--remote-platform", "unix"][..],
    ] {
      let mut args = vec!["ctl"];
      args.extend_from_slice(flags);
      args.extend(["components", "list"]);
      let arguments = Arguments::try_parse_from(args).unwrap();
      assert!(matches!(
        run(arguments).await,
        Err(CliError::ComponentsTarget)
      ));
    }
    assert!(
      Arguments::try_parse_from([
        "ctl",
        "components",
        "sync",
        "--from",
        "build",
        "--ctld-package",
        "package"
      ])
      .is_err()
    );
    assert!(
      Arguments::try_parse_from([
        "ctl",
        "components",
        "sync",
        "--from",
        "build",
        "--local-build",
        "--source",
        "ci"
      ])
      .is_err()
    );
  }

  #[tokio::test]
  async fn passwords_reject_remote_flags_before_preparing_helpers_or_connecting() {
    use clap::Parser;
    for flags in [
      vec!["--host", "work"],
      vec!["--method", "ssh"],
      vec!["--host", "work", "--remote-platform", "windows"],
    ] {
      for action in ["list", "remove", "clear"] {
        let arguments = Arguments::try_parse_from(
          [vec!["ctl"], flags.clone(), vec!["passwords", action]].concat(),
        )
        .unwrap();
        assert!(matches!(
          run(arguments).await,
          Err(CliError::PasswordsTarget)
        ));
      }
    }
  }

  #[tokio::test]
  async fn setup_rejects_remote_routing_before_download_or_connection() {
    use clap::Parser;
    for options in [
      vec!["--host", "work"],
      vec!["--host", "work", "--method", "ssh"],
      vec!["--host", "work", "--remote-platform", "windows"],
    ] {
      let arguments =
        Arguments::try_parse_from([vec!["ctl"], options, vec!["setup"]].concat()).unwrap();
      assert!(matches!(run(arguments).await, Err(CliError::SetupTarget)));
    }
  }

  #[tokio::test]
  async fn remote_vpn_profile_mutations_are_rejected_before_connecting() {
    use clap::Parser;

    for action in ["create", "remove"] {
      let arguments =
        Arguments::try_parse_from(["ctl", "--host", "vpn-server", "vpn", action]).unwrap();
      assert!(matches!(
        run(arguments).await,
        Err(CliError::Vpn(crate::vpn::Error::LocalProfilesOnly))
      ));
    }
  }

  #[tokio::test]
  async fn remote_daemon_restart_is_rejected_before_connecting() {
    use clap::Parser;
    let arguments =
      Arguments::try_parse_from(["ctl", "--host", "task-server", "taskd", "restart"]).unwrap();
    assert!(matches!(
      run(arguments).await,
      Err(CliError::RemoteTaskDaemonRestartUnsupported)
    ));
  }

  #[test]
  fn remote_interactive_sessions_keep_the_task_host_and_connection_options() {
    let target = ConnectionTarget::ssh_with_options(
      "task-server",
      ctl_client::SshConnectionOptions {
        remote_platform: ctl_client::RemotePlatform::Windows,
        hostname: Some("server.example".into()),
        user: Some("task-user".into()),
        port: Some(2222),
        identity_file: Some(PathBuf::from("task-key")),
        gateways: Vec::new(),
      },
    );
    let connector = CtlConnector {
      target: target.clone(),
      settings: ctl_client::hosts::ConnectionTargetDto::ssh("task-server"),
      recovery: Arc::default(),
      terminal_ui_active: Arc::default(),
    };
    let attachment = connector.for_interactive_session(PathBuf::from("/remote/ctmux.sock"));
    assert_eq!(attachment.target, target);
    assert!(Arc::ptr_eq(&attachment.recovery, &connector.recovery));
  }

  #[test]
  fn local_interactive_sessions_use_the_backend_socket() {
    let connector = CtlConnector {
      settings: ctl_client::hosts::ConnectionTargetDto::Local,
      recovery: Arc::default(),
      terminal_ui_active: Arc::default(),
      target: ConnectionTarget::Local {
        socket_path: PathBuf::from("/default/ctmux.sock"),
      },
    };
    let attachment = connector.for_interactive_session(PathBuf::from("/backend/ctmux.sock"));
    assert_eq!(
      attachment.target,
      ConnectionTarget::Local {
        socket_path: PathBuf::from("/backend/ctmux.sock"),
      }
    );
  }
}
