use ctl_client::setup::Error;
use ctl_ipc::resolver::{Intent, Request};

#[derive(Debug, clap::Args)]
pub struct Arguments {
  /// Print the selected helper's source, path, build, and archive hash as JSON.
  #[arg(long)]
  json: bool,
}

pub async fn run(arguments: Arguments) -> Result<(), Error> {
  if !arguments.json {
    eprintln!("Preparing the selected ctld helper...");
  }
  let selection = tokio::select! {
    result = ctl_ipc::resolver::resolve(Request::new(Intent::Setup)) => {
      result.map_err(|error| Error::Io(std::io::Error::other(error)))?
    }
    signal = tokio::signal::ctrl_c() => {
      signal?;
      return Err(Error::Io(std::io::Error::new(std::io::ErrorKind::Interrupted, "setup cancelled; no daemon was restarted")));
    }
  };
  if arguments.json {
    println!(
      "{}",
      serde_json::to_string(&serde_json::json!({
        "component": "ctld",
        "version": selection.info.as_ref().map(|info| &info.build.version),
        "executable": selection.executable,
        "reused": selection.reused,
        "selection": selection,
        "running_connections_preserved": true,
      }))
      .map_err(std::io::Error::other)?
    );
  } else {
    println!(
      "ctld selected from {} at {}",
      selection.source,
      selection.executable.display()
    );
    if let Some(info) = &selection.info {
      println!(
        "Build: {} {}",
        info.build.version, info.build.source_fingerprint
      );
    }
    if let Some(hash) = &selection.archive_sha256 {
      println!("Archive SHA-256: {hash}");
    }
    println!("Running connections were preserved. Restart ctld explicitly to use this helper.");
  }
  Ok(())
}
