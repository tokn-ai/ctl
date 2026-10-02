use std::fmt::Write as _;
use std::path::PathBuf;

mod inventory;
pub(crate) mod profiles;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
  /// List saved VPN profiles and runtime status without starting ctld.
  List {
    /// Print machine-readable JSON without credentials.
    #[arg(long)]
    json: bool,
  },
  /// Connect a saved VPN by name or ID, recreating its container when needed.
  Connect {
    #[arg(value_name = "NAME_OR_ID")]
    profile: String,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
  },
  /// Start the VPN and print its connection status.
  Start {
    /// Literal VPN settings file, resolved relative to the current directory.
    #[arg(long, default_value = ".env", value_name = "PATH")]
    env_file: PathBuf,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
  },
  /// Start a Tailscale container with browser sign-in and persistent identity.
  StartTailscale {
    /// Stable connection ID; reuse it to retain this node's login on restart.
    #[arg(long = "id", value_name = "CONNECTION_ID")]
    connection_id: String,
    #[arg(long, default_value = "Tailscale")]
    name: String,
    /// Optional device name shown in the tailnet.
    #[arg(long)]
    hostname: Option<String>,
    /// Allow access through subnet routes advertised in the tailnet.
    #[arg(long)]
    accept_routes: bool,
    /// Print machine-readable JSON, including any pending sign-in URL.
    #[arg(long)]
    json: bool,
  },
  /// Release this daemon's VPN connection, keeping ctld running.
  Stop {
    /// VPN ID from list; required when multiple VPNs are running.
    #[arg(value_name = "VPN_ID")]
    vpn_id: Option<String>,
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
    Command::Connect { profile, json } => {
      let connection = profiles::resolve(profiles::load(&profiles::path()?)?, &profile)?;
      (ctl_ipc::vpn::start_connection(connection).await?, json)
    }
    Command::Start { env_file, json } => (ctl_ipc::vpn::start(env_file).await?, json),
    Command::StartTailscale {
      connection_id,
      name,
      hostname,
      accept_routes,
      json,
    } => {
      let connection = ctl_ipc::VpnConnection {
        connection_id,
        name,
        settings: ctl_ipc::VpnSettings::Tailscale {
          hostname,
          accept_routes,
        },
      };
      (ctl_ipc::vpn::start_connection(connection).await?, json)
    }
    Command::Stop { vpn_id, json } => {
      let status = match vpn_id {
        Some(vpn_id) => ctl_ipc::vpn::stop_id(&vpn_id).await?,
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
    .any(|status| status.locally_connected.is_some() || status.shared_container)
  {
    let rows = rows.into_iter().enumerate().map(
      |(index, [id, provider, state, server, username, endpoint])| {
        let usage = match statuses[index].locally_connected {
          Some(true) => "this ctld",
          Some(false) => "shared",
          None => "-",
        };
        [
          id,
          provider,
          state,
          usage.into(),
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

fn display_value(value: Option<&str>) -> String {
  let value = value
    .filter(|value| !value.is_empty())
    .unwrap_or("unavailable");
  crate::table::text(value)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error(transparent)]
  Profile(#[from] profiles::Error),
  #[error(transparent)]
  Vpn(#[from] ctl_ipc::vpn::VpnError),
  #[error(transparent)]
  Json(#[from] serde_json::Error),
}
