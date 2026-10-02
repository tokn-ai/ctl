use ctl_client::setup::{self, Error, SetupEvent};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, clap::Args)]
pub struct Arguments {
  /// Print the installed helper path and version as JSON.
  #[arg(long)]
  json: bool,
}

pub async fn run(arguments: Arguments) -> Result<(), Error> {
  let progress = Mutex::new((None::<Instant>, Instant::now()));
  let outcome = tokio::select! {
    result = setup::install_signed_ctld(|event| {
      if arguments.json { return; }
      match event {
        SetupEvent::Manifest => eprintln!("Fetching signed ctld release metadata..."),
        SetupEvent::Extracting => eprintln!("Extracting ctld.app..."),
        SetupEvent::Verifying => eprintln!("Verifying Apple signature, notarization, and helper protocol..."),
        SetupEvent::Activating => eprintln!("Selecting the verified helper..."),
        SetupEvent::Downloading { received_bytes, total_bytes } => {
          let mut progress = progress.lock().unwrap();
          let started = *progress.0.get_or_insert_with(Instant::now);
          if progress.1.elapsed() >= Duration::from_secs(1) || received_bytes == total_bytes {
            // Downloads are bounded to 128 MiB by the installer.
            let speed = f64::from(u32::try_from(received_bytes).unwrap_or(u32::MAX)) / started.elapsed().as_secs_f64().max(0.001);
            eprintln!("Downloading ctld.app: {received_bytes}/{total_bytes} bytes ({speed:.0} bytes/s)");
            progress.1 = Instant::now();
          }
        }
      }
    }) => result?,
    signal = tokio::signal::ctrl_c() => {
      signal?;
      return Err(Error::Io(std::io::Error::new(std::io::ErrorKind::Interrupted, "setup cancelled; no daemon was restarted")));
    }
  };
  if arguments.json {
    println!(
      "{}",
      serde_json::to_string(&outcome).map_err(std::io::Error::other)?
    );
  } else {
    println!(
      "ctld {} is installed at {}",
      outcome.version,
      outcome.executable.display()
    );
    println!(
      "Running connections were preserved. Restart ctld explicitly to use the selected helper."
    );
    if std::env::var_os("CTLD_BIN").is_some() {
      println!("CTLD_BIN is set and continues to override the managed helper.");
    }
  }
  Ok(())
}
