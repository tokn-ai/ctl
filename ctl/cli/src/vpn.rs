use ctl_client::hosts::ConnectionTargetDto;
use std::fmt::Write as _;
mod inventory;
pub(crate) mod profiles;
mod questionnaire;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
  /// List saved VPN profiles and runtime status on the selected owner.
  List {
    /// Print machine-readable JSON without credentials.
    #[arg(long)]
    json: bool,
  },
  /// Create a saved VPN profile with an interactive questionnaire.
  Create {
    /// Print the saved profile's metadata as JSON, keeping prompts on stderr.
    #[arg(long)]
    json: bool,
  },
  /// Remove a stopped saved VPN profile after interactive confirmation.
  Remove {
    #[arg(value_name = "NAME_OR_ID")]
    profile: Option<String>,
    /// Print removed profile metadata as JSON, keeping confirmation on stderr.
    #[arg(long)]
    json: bool,
  },
  /// Start a saved VPN by name or ID, or choose one interactively.
  Start {
    #[arg(value_name = "NAME_OR_ID")]
    profile: Option<String>,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
  },
  /// Release this daemon's VPN connection, keeping ctld running.
  Stop {
    /// Saved profile name or ID, or a runtime VPN ID from list.
    #[arg(value_name = "NAME_OR_ID")]
    profile: Option<String>,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
  },
}

pub(crate) enum RuntimeClient {
  Local(ctl_ipc::vpn::Client),
  Remote(ctl_ipc::remote_vpn::Client),
}

impl RuntimeClient {
  async fn for_target(target: &ConnectionTargetDto, start_routes: bool) -> Result<Self, Error> {
    if matches!(target, ConnectionTargetDto::Local) {
      return Ok(Self::Local(ctl_ipc::vpn::Client::default()));
    }
    #[cfg(unix)]
    {
      if start_routes {
        crate::target::ensure_vpn(target)
          .await
          .map_err(|error| Error::Preparation(Box::new(error)))?;
      }
      let owner = target.to_ssh_target()?;
      let control_path = crate::ssh_broker::ensure_master(owner.clone()).await?;
      let expected_remote_id = match target {
        ConnectionTargetDto::Ssh { remote_info, .. } => remote_info
          .as_ref()
          .map(|identity| identity.remote_id.clone()),
        ConnectionTargetDto::Local => None,
      };
      Ok(Self::Remote(
        ctl_ipc::remote_vpn::Client::new(owner, expected_remote_id).with_control_path(control_path),
      ))
    }
    #[cfg(not(unix))]
    {
      let _ = start_routes;
      Err(Error::RemoteVpnUnsupported)
    }
  }

  pub(crate) async fn list(&self) -> Result<ctl_ipc::VpnSnapshot, Error> {
    match self {
      Self::Local(client) => Ok(client.list().await?),
      Self::Remote(client) => Ok(client.list().await?),
    }
  }

  pub(crate) async fn start_connection(
    &self,
    connection: ctl_ipc::VpnConnection,
  ) -> Result<ctl_ipc::VpnStatus, Error> {
    match self {
      Self::Local(client) => Ok(client.start_connection(connection).await?),
      Self::Remote(client) => Ok(client.start_connection(connection).await?),
    }
  }

  async fn stop_id(&self, connection_id: &str) -> Result<ctl_ipc::VpnStatus, Error> {
    match self {
      Self::Local(client) => Ok(client.stop_id(connection_id).await?),
      Self::Remote(client) => Ok(client.stop_id(connection_id).await?),
    }
  }

  async fn stop(&self) -> Result<ctl_ipc::VpnStatus, Error> {
    match self {
      Self::Local(client) => Ok(client.stop().await?),
      Self::Remote(_) => {
        let snapshot = self.list().await?;
        let mut connections = snapshot.connections.iter().filter(|status| {
          status.locally_connected != Some(false)
            && (status.running || status.state != ctl_ipc::VpnState::Stopped)
        });
        let Some(connection) = connections.next() else {
          if !snapshot.discovery_warnings.is_empty() {
            return Err(Error::InventoryUnavailable);
          }
          return Ok(ctl_ipc::VpnStatus::default());
        };
        if connections.next().is_some() {
          return Err(Error::AmbiguousStop);
        }
        let id = connection
          .vpn_id
          .as_deref()
          .ok_or(Error::InventoryUnavailable)?;
        self.stop_id(id).await
      }
    }
  }
}

pub async fn run(command: Command, target: &ConnectionTargetDto) -> Result<(), Error> {
  if matches!(command, Command::Create { .. } | Command::Remove { .. })
    && !matches!(target, ConnectionTargetDto::Local)
  {
    return Err(Error::LocalProfilesOnly);
  }
  if let Command::Create { json } = command {
    return create(json).await;
  }
  if let Command::Remove { profile, json } = command {
    return remove(profile, json).await;
  }
  let (status, json) = match command {
    Command::List { json } => {
      let client = RuntimeClient::for_target(target, false).await?;
      list(json, &client).await?;
      return Ok(());
    }
    Command::Create { .. } | Command::Remove { .. } => {
      unreachable!("profile operations handled above")
    }
    Command::Start { profile, json } => {
      let Some(connection) = worker(move || choose_saved(profile, "start")).await? else {
        return Ok(());
      };
      let client = RuntimeClient::for_target(target, true).await?;
      (client.start_connection(connection).await?, json)
    }
    Command::Stop { profile, json } => {
      let client = RuntimeClient::for_target(target, false).await?;
      let status = match profile {
        Some(profile) => {
          let id = worker(move || stop_id(profile)).await?;
          client.stop_id(&id).await?
        }
        None if questionnaire::available() => {
          match stop_options(&client).await? {
            Some(options) => {
              let Some(id) = worker(move || questionnaire::pick("stop", options)).await? else {
                return Ok(());
              };
              client.stop_id(&id).await?
            }
            // Legacy daemons cannot target a stop by ID. Their inventory is
            // limited to one local connection, so retain untargeted stop.
            None => client.stop().await?,
          }
        }
        // Preserve safe zero/one-connection behavior for scripts. The daemon
        // rejects an ambiguous untargeted stop rather than stopping all VPNs.
        None => client.stop().await?,
      };
      (status, json)
    }
  };
  if json {
    println!("{}", serde_json::to_string(&status)?);
  } else {
    println!("{}", format_statuses(std::slice::from_ref(&status)));
  }
  Ok(())
}

async fn worker<T: Send + 'static>(
  operation: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
  tokio::task::spawn_blocking(operation)
    .await
    .map_err(|_| Error::QuestionnaireWorkerStopped)?
}

async fn create(json: bool) -> Result<(), Error> {
  if !questionnaire::available() {
    return Err(Error::TerminalRequired("create"));
  }
  let Some(connection) = worker(|| {
    let path = profiles::path()?;
    let names = profiles::load(&path)?
      .connections
      .into_iter()
      .map(|connection| connection.name)
      .collect();
    let Some(connection) = questionnaire::create(names)? else {
      return Ok(None);
    };
    profiles::create(&path, &connection)?;
    cliclack::outro("VPN profile saved.")?;
    Ok(Some(connection))
  })
  .await?
  else {
    return Ok(());
  };
  let id = connection.connection_id.clone();
  let document = ctl_client::hosts::SavedVpnDocument {
    connections: vec![connection],
    ..ctl_client::hosts::SavedVpnDocument::default()
  };
  let entries = inventory::entries(Some(&document), &ctl_ipc::VpnSnapshot::default());
  if json {
    println!("{}", serde_json::to_string(&entries[0])?);
  } else {
    println!(
      "{}\n\nStart with: ctl vpn start {id}",
      inventory::format(&entries)
    );
  }
  Ok(())
}

fn choose_saved(
  profile: Option<String>,
  action: &'static str,
) -> Result<Option<ctl_ipc::VpnConnection>, Error> {
  if profile.is_none() && !questionnaire::available() {
    return Err(Error::TerminalRequired(action));
  }
  let document = profiles::load(&profiles::path()?)?;
  if let Some(profile) = profile {
    return Ok(Some(profiles::resolve(document, &profile)?));
  }
  if document.connections.is_empty() {
    return Err(Error::NoSavedProfiles);
  }
  let options = document
    .connections
    .iter()
    .map(|connection| {
      (
        connection.connection_id.clone(),
        format!(
          "{} ({})",
          connection.name,
          provider_name(connection.provider())
        ),
        connection.connection_id.clone(),
      )
    })
    .collect();
  drop(document);
  let Some(id) = questionnaire::pick(action, options)? else {
    return Ok(None);
  };
  // The desktop may edit or delete a profile while the picker is open.
  // Resolve the stable ID again so the next operation uses current settings.
  Ok(Some(
    profiles::load(&profiles::path()?)?
      .connections
      .into_iter()
      .find(|connection| connection.connection_id == id)
      .ok_or(profiles::Error::NotFound(id))?,
  ))
}

#[derive(serde::Serialize)]
struct RemovedProfile {
  removed: bool,
  connection_id: String,
  name: String,
  provider: ctl_ipc::VpnProvider,
}

async fn remove(profile: Option<String>, json: bool) -> Result<(), Error> {
  if !questionnaire::available() {
    return Err(Error::TerminalRequired("remove"));
  }
  let Some(connection) = worker(move || {
    let Some(connection) = choose_saved(profile, "remove")? else {
      return Ok(None);
    };
    Ok(questionnaire::confirm_remove(&connection)?.then_some(connection))
  })
  .await?
  else {
    return Ok(());
  };
  #[cfg(unix)]
  require_removable(&connection, &ctl_ipc::vpn::list().await?)?;
  let removed = worker(move || {
    profiles::remove(&profiles::path()?, &connection)?;
    cliclack::outro("VPN profile removed.")?;
    Ok(RemovedProfile {
      removed: true,
      provider: connection.provider(),
      connection_id: connection.connection_id,
      name: connection.name,
    })
  })
  .await?;
  if json {
    println!("{}", serde_json::to_string(&removed)?);
  } else {
    println!(
      "Removed VPN profile {} ({}).",
      crate::table::text(&removed.name),
      crate::table::text(&removed.connection_id)
    );
  }
  Ok(())
}

#[cfg(unix)]
fn require_removable(
  connection: &ctl_ipc::VpnConnection,
  snapshot: &ctl_ipc::VpnSnapshot,
) -> Result<(), Error> {
  if !snapshot.discovery_warnings.is_empty() || !snapshot.supports_multiple {
    return Err(Error::RemovalInventoryUnavailable);
  }
  let id = Some(connection.connection_id.as_str());
  if snapshot.connections.iter().any(|status| {
    let matches = status.connection_id.as_deref() == id
      || (status.connection_id.is_none()
        && !status.shared_container
        && status.vpn_id.as_deref() == id);
    matches
      && (status.running || status.state != ctl_ipc::VpnState::Stopped || status.status_unavailable)
  }) {
    return Err(Error::ProfileActive(connection.connection_id.clone()));
  }
  Ok(())
}

fn stop_id(selector: String) -> Result<String, Error> {
  let document = profiles::path().and_then(|path| profiles::load(&path));
  match document {
    Ok(document) => match profiles::resolve(document, &selector) {
      Ok(connection) => Ok(connection.connection_id),
      Err(profiles::Error::NotFound(_)) => Ok(selector),
      Err(error) => Err(error.into()),
    },
    // An unreadable catalog must not prevent releasing an exact runtime ID.
    Err(_) => Ok(selector),
  }
}

async fn stop_options(
  client: &RuntimeClient,
) -> Result<Option<Vec<(String, String, String)>>, Error> {
  let snapshot = client.list().await?;
  let document = profiles::path().and_then(|path| profiles::load(&path)).ok();
  let entries = inventory::entries(document.as_ref(), &snapshot);
  let options: Vec<_> = snapshot
    .connections
    .iter()
    .filter_map(|status| {
      if status.locally_connected == Some(false) {
        return None;
      }
      let id = status.vpn_id.as_ref()?;
      let name = entries
        .iter()
        .find(|entry| entry.vpn_id.as_ref() == Some(id) && entry.provider == status.provider)
        .and_then(|entry| entry.name.as_deref())
        .unwrap_or(id);
      Some((
        id.clone(),
        format!("{name} ({})", provider_name(status.provider)),
        id.clone(),
      ))
    })
    .collect();
  if options.is_empty() {
    if !snapshot.discovery_warnings.is_empty() {
      return Err(Error::InventoryUnavailable);
    }
    return Err(Error::NoOwnedVpn);
  }
  Ok(snapshot.supports_multiple.then_some(options))
}

#[derive(serde::Serialize)]
struct InventorySnapshot {
  #[serde(flatten)]
  runtime: ctl_ipc::VpnSnapshot,
  entries: Vec<inventory::Entry>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  profile_warnings: Vec<String>,
}

async fn list(json: bool, client: &RuntimeClient) -> Result<(), Error> {
  let mut profile_warnings = Vec::new();
  let document = match profiles::path().and_then(|path| profiles::load(&path)) {
    Ok(document) => Some(document),
    Err(error) => {
      profile_warnings.push(format!("Could not load saved VPN profiles: {error}"));
      None
    }
  };
  // Keep either source visible when the other cannot be observed. This query
  // never starts ctld or acquires an interest in a shared container.
  let runtime = client
    .list()
    .await
    .unwrap_or_else(|error| ctl_ipc::VpnSnapshot {
      discovery_warnings: vec![format!("Could not inspect VPN runtime status: {error}")],
      ..ctl_ipc::VpnSnapshot::default()
    });
  let snapshot = InventorySnapshot {
    entries: inventory::entries(document.as_ref(), &runtime),
    runtime,
    profile_warnings,
  };
  let output = if json {
    serde_json::to_string(&snapshot)?
  } else {
    let mut output = if snapshot.entries.is_empty()
      && (!snapshot.runtime.discovery_warnings.is_empty() || !snapshot.profile_warnings.is_empty())
    {
      "VPN inventory unavailable.".into()
    } else {
      inventory::format(&snapshot.entries)
    };
    append_notes(&mut output, &snapshot.runtime.connections);
    if !snapshot.runtime.supports_multiple
      && snapshot
        .entries
        .iter()
        .any(|entry| entry.saved && entry.state == inventory::State::Unavailable)
    {
      output.push_str(
        "\n\nNote: This ctld reports only local VPN status. Update ctld for complete inventory.",
      );
    }
    for warning in snapshot
      .runtime
      .discovery_warnings
      .iter()
      .chain(&snapshot.profile_warnings)
    {
      let _ = write!(output, "\n\nWarning: {}", crate::table::text(warning));
    }
    output
  };
  println!("{output}");
  Ok(())
}

fn sign_in_url(status: &ctl_ipc::VpnStatus) -> Option<&str> {
  if status.provider == ctl_ipc::VpnProvider::Tailscale
    && status.state == ctl_ipc::VpnState::Starting
    && !status.status_unavailable
  {
    status
      .auth_url
      .as_deref()
      .filter(|url| ctl_ipc::vpn::is_tailscale_auth_url(url))
  } else {
    None
  }
}

fn append_notes(output: &mut String, statuses: &[ctl_ipc::VpnStatus]) {
  for status in statuses {
    if let Some(url) = sign_in_url(status) {
      let _ = write!(
        output,
        "\n\nSign in for {}: {url}",
        display_value(status.vpn_id.as_deref())
      );
    } else if let Some(message) = status
      .message
      .as_deref()
      .filter(|message| !message.is_empty())
    {
      let _ = write!(
        output,
        "\n\n{}: {}",
        display_value(status.vpn_id.as_deref()),
        display_value(Some(message))
      );
    }
  }
}

fn provider_name(provider: ctl_ipc::VpnProvider) -> &'static str {
  match provider {
    ctl_ipc::VpnProvider::Openconnect => "OpenConnect",
    ctl_ipc::VpnProvider::Tailscale => "Tailscale",
  }
}

fn status_row(status: &ctl_ipc::VpnStatus) -> [String; 6] {
  use ctl_ipc::VpnState;

  let state = if status.status_unavailable {
    "unavailable"
  } else {
    match status.state {
      VpnState::Stopped => "disconnected",
      VpnState::Starting if sign_in_url(status).is_some() => "sign-in required",
      VpnState::Starting => "starting",
      VpnState::Connected => "connected",
      VpnState::Stopping => "stopping",
    }
  };
  let vpn_id = display_value(status.vpn_id.as_deref().or(Some("-")));
  let provider = provider_name(status.provider);
  if status.state == VpnState::Stopped {
    [
      vpn_id,
      "-".into(),
      state.to_owned(),
      "-".into(),
      "-".into(),
      "-".into(),
    ]
  } else {
    [
      vpn_id,
      provider.to_owned(),
      state.to_owned(),
      display_value(status.tailnet.as_deref().or(status.vpn_url.as_deref())),
      display_value(status.username.as_deref()),
      display_value(status.endpoint.as_deref()),
    ]
  }
}

fn format_statuses(statuses: &[ctl_ipc::VpnStatus]) -> String {
  let rows: Vec<_> = if statuses.is_empty() {
    vec![status_row(&ctl_ipc::VpnStatus::default())]
  } else {
    statuses.iter().map(status_row).collect()
  };
  let headers = [
    "VPN ID",
    "PROVIDER",
    "STATE",
    "SERVER",
    "USERNAME",
    "SOCKS5 ENDPOINT",
  ];
  let mut table = if statuses
    .iter()
    .any(|status| status.locally_connected == Some(false))
  {
    let rows = rows.into_iter().zip(statuses).map(
      |([id, provider, state, server, username, endpoint], status)| {
        [
          id,
          provider,
          state,
          usage_label(status.locally_connected).into(),
          server,
          username,
          endpoint,
        ]
      },
    );
    crate::table::format(
      [
        "VPN ID",
        "PROVIDER",
        "STATE",
        "USE",
        "SERVER",
        "USERNAME",
        "SOCKS5 ENDPOINT",
      ],
      rows,
    )
  } else {
    crate::table::format(headers, rows)
  };
  append_notes(&mut table, statuses);
  table
}

fn usage_label(locally_connected: Option<bool>) -> &'static str {
  match locally_connected {
    Some(true) => "owned",
    Some(false) => "shared",
    None => "-",
  }
}

fn display_value(value: Option<&str>) -> String {
  let value = value
    .filter(|value| !value.is_empty())
    .unwrap_or("unavailable");
  crate::table::text(value)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("VPN profile creation and removal are local operations; omit --host.")]
  LocalProfilesOnly,
  #[error("Multiple remote VPN connections are active; specify a VPN name or ID to stop one.")]
  AmbiguousStop,
  #[cfg(not(unix))]
  #[error("Remote VPN management requires a Unix client.")]
  RemoteVpnUnsupported,
  #[error(transparent)]
  Host(#[from] ctl_client::hosts::HostError),
  #[cfg(unix)]
  #[error(transparent)]
  Broker(#[from] crate::ssh_broker::Error),
  #[error(transparent)]
  Remote(#[from] ctl_ipc::remote_vpn::Error),
  #[error(transparent)]
  Preparation(Box<crate::target::Error>),
  #[error("An interactive terminal is required for `ctl vpn {0}`.")]
  TerminalRequired(&'static str),
  #[error("No saved VPN profiles.")]
  NoSavedProfiles,
  #[error("No owned VPNs are running.")]
  NoOwnedVpn,
  #[error("VPN runtime inventory is unavailable. Specify a VPN name or ID to stop one.")]
  InventoryUnavailable,
  #[error("VPN runtime inventory is incomplete. Start or update ctld, then retry removal.")]
  RemovalInventoryUnavailable,
  #[error(
    "VPN {0:?} is active. Stop it and wait for its container to exit before removing its profile."
  )]
  ProfileActive(String),
  #[error("The VPN questionnaire stopped unexpectedly.")]
  QuestionnaireWorkerStopped,
  #[error("Could not complete the VPN questionnaire: {0}")]
  Questionnaire(#[from] std::io::Error),
  #[error(transparent)]
  Profile(#[from] profiles::Error),
  #[error(transparent)]
  Vpn(#[from] ctl_ipc::vpn::VpnError),
  #[error(transparent)]
  Json(#[from] serde_json::Error),
}
