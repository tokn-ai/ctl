use std::path::PathBuf;

use unicode_width::UnicodeWidthStr as _;

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
  /// Print every managed VPN connection without starting ctld.
  Status {
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
  },
  /// Stop a VPN connection, keeping ctld running.
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
    Command::Status { json } => {
      let snapshot = ctld_ipc::vpn::list().await?;
      let output = if json {
        serde_json::to_string(&snapshot)?
      } else {
        format_statuses(&snapshot.connections)
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

fn status_row(status: &ctld_ipc::VpnStatus) -> [String; 5] {
  use ctld_ipc::VpnState;

  let state = match status.state {
    VpnState::Stopped => "disconnected",
    VpnState::Starting => "starting",
    VpnState::Connected => "connected",
    VpnState::Stopping => "stopping",
  };
  let vpn_id = display_value(status.vpn_id.as_deref().or(Some("-")));
  if status.state == VpnState::Stopped {
    [vpn_id, state.to_owned(), "-".into(), "-".into(), "-".into()]
  } else {
    [
      vpn_id,
      state.to_owned(),
      display_value(status.vpn_url.as_deref()),
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
  let headers = ["VPN ID", "STATE", "SERVER", "USERNAME", "SOCKS5 ENDPOINT"];
  let widths = std::array::from_fn(|index| {
    rows
      .iter()
      .map(|row| row[index].width())
      .fold(headers[index].width(), usize::max)
  });
  let mut table = format_row(headers, widths);
  for row in &rows {
    table.push('\n');
    table.push_str(&format_row(row.each_ref().map(String::as_str), widths));
  }
  table
}

fn format_row<const COLUMNS: usize>(cells: [&str; COLUMNS], widths: [usize; COLUMNS]) -> String {
  let mut row = String::new();
  for (index, cell) in cells.into_iter().enumerate() {
    row.push_str(cell);
    if index + 1 < cells.len() {
      row.push_str(&" ".repeat(widths[index] - cell.width() + 2));
    }
  }
  row
}

fn display_value(value: Option<&str>) -> String {
  let value = value
    .filter(|value| !value.is_empty())
    .unwrap_or("unavailable");
  let mut rendered = String::with_capacity(value.len());
  for character in value.chars() {
    if character.is_control()
      || matches!(character, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    {
      rendered.extend(character.escape_default());
    } else {
      rendered.push(character);
    }
  }
  rendered
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error(transparent)]
  Vpn(#[from] ctld_ipc::vpn::VpnError),
  #[error(transparent)]
  Json(#[from] serde_json::Error),
}
