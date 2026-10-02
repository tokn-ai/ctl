use std::fmt::Write as _;
mod inventory;
pub(crate) mod profiles;
mod questionnaire;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
  /// List saved VPN profiles and runtime status without starting ctld.
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

pub async fn run(command: Command) -> Result<(), Error> {
  let (status, json) = match command {
    Command::List { json } => {
      list(json).await?;
      return Ok(());
    }
    Command::Create { json } => {
      create(json).await?;
      return Ok(());
    }
    Command::Start { profile, json } => {
      let Some(connection) = worker(move || choose_start(profile)).await? else {
        return Ok(());
      };
      (ctl_ipc::vpn::start_connection(connection).await?, json)
    }
    Command::Stop { profile, json } => {
      let status = match profile {
        Some(profile) => {
          let id = worker(move || stop_id(profile)).await?;
          ctl_ipc::vpn::stop_id(&id).await?
        }
        None if questionnaire::available() => {
          match stop_options().await? {
            Some(options) => {
              let Some(id) = worker(move || questionnaire::pick("stop", options)).await? else {
                return Ok(());
              };
              ctl_ipc::vpn::stop_id(&id).await?
            }
            // Legacy daemons cannot target a stop by ID. Their inventory is
            // limited to one local connection, so retain untargeted stop.
            None => ctl_ipc::vpn::stop().await?,
          }
        }
        // Preserve safe zero/one-connection behavior for scripts. The daemon
        // rejects an ambiguous untargeted stop rather than stopping all VPNs.
        None => ctl_ipc::vpn::stop().await?,
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

fn choose_start(profile: Option<String>) -> Result<Option<ctl_ipc::VpnConnection>, Error> {
  if profile.is_none() && !questionnaire::available() {
    return Err(Error::TerminalRequired("start"));
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
  let Some(id) = questionnaire::pick("start", options)? else {
    return Ok(None);
  };
  // The desktop may edit or delete a profile while the picker is open.
  // Resolve the stable ID again so starting uses the current saved settings.
  Ok(Some(
    profiles::load(&profiles::path()?)?
      .connections
      .into_iter()
      .find(|connection| connection.connection_id == id)
      .ok_or(profiles::Error::NotFound(id))?,
  ))
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

async fn stop_options() -> Result<Option<Vec<(String, String, String)>>, Error> {
  let snapshot = ctl_ipc::vpn::list().await?;
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

async fn list(json: bool) -> Result<(), Error> {
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
  let runtime = ctl_ipc::vpn::list()
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
  #[error("An interactive terminal is required for `ctl vpn {0}`.")]
  TerminalRequired(&'static str),
  #[error("No saved VPN profiles. Run `ctl vpn create` first.")]
  NoSavedProfiles,
  #[error("No owned VPNs are running.")]
  NoOwnedVpn,
  #[error("VPN runtime inventory is unavailable. Specify a VPN name or ID to stop one.")]
  InventoryUnavailable,
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
