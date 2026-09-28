use std::path::PathBuf;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
  /// Start the VPN and print its SOCKS5 endpoint as JSON.
  Start {
    /// Literal VPN settings file, resolved relative to the current directory.
    #[arg(long, default_value = ".env", value_name = "PATH")]
    env_file: PathBuf,
  },
  /// Print VPN status as JSON without starting ctld.
  Status,
  /// Stop the owned VPN container, keeping ctld running.
  Stop,
}

pub async fn run(command: Command) -> Result<(), Error> {
  let status = match command {
    Command::Start { env_file } => ctld_ipc::vpn::start(env_file).await?,
    Command::Status => ctld_ipc::vpn::status().await?,
    Command::Stop => ctld_ipc::vpn::stop().await?,
  };
  println!("{}", serde_json::to_string(&status)?);
  Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error(transparent)]
  Vpn(#[from] ctld_ipc::vpn::VpnError),
  #[error(transparent)]
  Json(#[from] serde_json::Error),
}
