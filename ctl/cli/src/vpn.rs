use std::fmt::Write as _;
use std::path::PathBuf;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
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
  /// Print local connections and shared VPN containers without starting ctld.
  Status {
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
  },
  /// Release this daemon's VPN connection, keeping ctld running.
  Stop {
    /// VPN ID from status; required when multiple VPNs are running.
    #[arg(value_name = "VPN_ID")]
    vpn_id: Option<String>,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
  },
}

pub async fn run(command: Command) -> Result<(), Error> {
  let (status, json) = match command {
    Command::Start { env_file, json } => (ctld_ipc::vpn::start(env_file).await?, json),
    Command::StartTailscale {
      connection_id,
      name,
      hostname,
      accept_routes,
      json,
    } => {
      let connection = ctld_ipc::VpnConnection {
        connection_id,
        name,
        settings: ctld_ipc::VpnSettings::Tailscale {
          hostname,
          accept_routes,
        },
      };
      (ctld_ipc::vpn::start_connection(connection).await?, json)
    }
    Command::Status { json } => {
      let snapshot = ctld_ipc::vpn::list().await?;
      let output = if json {
        serde_json::to_string(&snapshot)?
      } else {
        let mut output =
          if snapshot.connections.is_empty() && !snapshot.discovery_warnings.is_empty() {
            "VPN inventory unavailable.".into()
          } else {
            format_statuses(&snapshot.connections)
          };
        for warning in &snapshot.discovery_warnings {
          let _ = write!(output, "\n\nWarning: {}", crate::table::text(warning));
        }
        output
      };
      println!("{output}");
      return Ok(());
    }
    Command::Stop { vpn_id, json } => {
      let status = match vpn_id {
        Some(vpn_id) => ctld_ipc::vpn::stop_id(&vpn_id).await?,
        None => ctld_ipc::vpn::stop().await?,
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

fn status_row(status: &ctld_ipc::VpnStatus) -> [String; 6] {
  use ctld_ipc::VpnState;

  let state = if status.status_unavailable {
    "unavailable"
  } else {
    match status.state {
      VpnState::Stopped => "disconnected",
      VpnState::Starting if status.auth_url.is_some() => "sign-in required",
      VpnState::Starting => "starting",
      VpnState::Connected => "connected",
      VpnState::Stopping => "stopping",
    }
  };
  let vpn_id = display_value(status.vpn_id.as_deref().or(Some("-")));
  let provider = match status.provider {
    ctld_ipc::VpnProvider::Openconnect => "OpenConnect",
    ctld_ipc::VpnProvider::Tailscale => "Tailscale",
  };
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

fn format_statuses(statuses: &[ctld_ipc::VpnStatus]) -> String {
  let rows: Vec<_> = if statuses.is_empty() {
    vec![status_row(&ctld_ipc::VpnStatus::default())]
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
  for status in statuses {
    if let Some(url) = &status.auth_url
      && !status.status_unavailable
      && ctld_ipc::vpn::is_tailscale_auth_url(url)
    {
      let _ = write!(
        table,
        "\n\nSign in for {}: {url}",
        display_value(status.vpn_id.as_deref())
      );
    } else if let Some(message) = status
      .message
      .as_deref()
      .filter(|message| !message.is_empty())
    {
      let _ = write!(
        table,
        "\n\n{}: {}",
        display_value(status.vpn_id.as_deref()),
        display_value(Some(message))
      );
    }
  }
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
  Vpn(#[from] ctld_ipc::vpn::VpnError),
  #[error(transparent)]
  Json(#[from] serde_json::Error),
}
