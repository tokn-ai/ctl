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
  /// Print VPN status without starting ctld.
  Status {
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
  },
  /// Stop the owned VPN container, keeping ctld running.
  Stop {
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
  },
}

pub async fn run(command: Command) -> Result<(), Error> {
  let (status, json) = match command {
    Command::Start { env_file, json } => (ctld_ipc::vpn::start(env_file).await?, json),
    Command::Status { json } => (ctld_ipc::vpn::status().await?, json),
    Command::Stop { json } => (ctld_ipc::vpn::stop().await?, json),
  };
  if json {
    println!("{}", serde_json::to_string(&status)?);
  } else {
    println!("{}", format_status(&status));
  }
  Ok(())
}

fn format_status(status: &ctld_ipc::VpnStatus) -> String {
  use ctld_ipc::VpnState;

  let state = match status.state {
    VpnState::Stopped => return "VPN: disconnected".into(),
    VpnState::Starting => "starting",
    VpnState::Connected => "connected",
    VpnState::Stopping => "stopping",
  };
  format!(
    "VPN: {state}\nServer: {}\nUsername: {}\nSOCKS5 proxy: {}",
    display_value(status.vpn_url.as_deref()),
    display_value(status.username.as_deref()),
    display_value(status.endpoint.as_deref()),
  )
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
