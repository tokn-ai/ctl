use super::arguments::DevelopmentArguments;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

const NATIVE_BUILDER: &str = "scripts/dev/build-local-development.mts";

pub(super) async fn run(home: &Path, options: DevelopmentArguments) -> io::Result<()> {
  let targets = options.destination.resolve()?;
  let remote = targets[0].remote();
  if remote
    && (options.app
      || options.ctream
      || options.release
      || options.profile.is_some()
      || options.target_dir.is_some()
      || options.target.is_some()
      || options.cargo_args_json.is_some()
      || options.lease)
  {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "Desktop, ctream, and native Cargo options require a local build. Remote development accepts host options, --workspace, and --json.",
    ));
  }
  let workspace = checkout_root(
    options.workspace.as_deref(),
    checkout_requirements(&options, remote),
  )?;
  if remote {
    return super::update::run_development(home, &targets, &workspace, options.json).await;
  }
  if options.app {
    return install_app(home, &workspace, options.json).await;
  }
  if options.ctream {
    return super::ctream::install_development(home, &workspace, &options).await;
  }
  let lease_input = options.lease.then(lease_pipe).transpose()?;
  let installation = ctl_client::development::install(
    home,
    ctl_client::development::DevelopmentOptions {
      workspace,
      cargo_args: cargo_arguments(&options)?,
    },
  );
  let input = async {
    if let Some(input) = lease_input {
      read_lease_input(input).await
    } else {
      std::future::pending().await
    }
  };
  tokio::pin!(input);
  let prepared = tokio::select! {
    result = installation => result?,
    result = &mut input => {
      result?;
      return Err(io::Error::new(io::ErrorKind::Interrupted,
        "Development launcher disconnected. Running services were preserved."));
    },
    _ = tokio::signal::ctrl_c() => return Err(io::Error::new(io::ErrorKind::Interrupted,
      "Development preparation cancelled. Running services were preserved.")),
  };
  let result = &prepared.result;
  if options.json {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, result).map_err(io::Error::other)?;
    writeln!(output)?;
    output.flush()?;
  } else {
    println!(
      "Installed development build {} ({}). Running services were preserved; restart separately.",
      result.bundle_id, result.target_triple
    );
  }
  if options.lease {
    tokio::select! {
      result = &mut input => result?,
      _ = tokio::signal::ctrl_c() => {},
    };
  }
  drop(prepared);
  Ok(())
}

pub(super) fn in_native_checkout() -> bool {
  checkout_root(None, &[NATIVE_BUILDER]).is_ok()
}

fn checkout_requirements(options: &DevelopmentArguments, remote: bool) -> &'static [&'static str] {
  if remote {
    &["scripts/dev/build-agent-bundle.mts"]
  } else if options.app {
    &[
      "scripts/dev/build-desktop-app.mts",
      "apps/desktop/src-tauri/tauri.conf.json",
    ]
  } else if options.ctream {
    &[
      "ctream/preview/Cargo.toml",
      "ctream/cli/Cargo.toml",
      "ctream/agent/Cargo.toml",
    ]
  } else {
    &[NATIVE_BUILDER]
  }
}

fn checkout_root(explicit: Option<&Path>, required: &[&str]) -> io::Result<PathBuf> {
  let directory = explicit.map_or_else(std::env::current_dir, |path| Ok(path.to_owned()))?;
  let workspace = directory.canonicalize().map_err(|error| {
    io::Error::new(
      error.kind(),
      format!("could not open the ctl checkout root: {error}"),
    )
  })?;
  if !workspace.join("Cargo.toml").is_file()
    || required.iter().any(|file| !workspace.join(file).is_file())
  {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "run ctlup dev from a ctl checkout root or pass its path with --workspace",
    ));
  }
  Ok(workspace)
}

async fn install_app(home: &Path, workspace: &Path, json: bool) -> io::Result<()> {
  #[cfg(any(target_os = "macos", target_os = "linux", windows))]
  {
    tokio::select! {
      result = super::app::install_development(home, workspace, json) => result,
      _ = tokio::signal::ctrl_c() => Err(io::Error::new(
        io::ErrorKind::Interrupted,
        "Desktop development build cancelled.",
      )),
    }
  }
  #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
  {
    let _ = (home, workspace, json);
    Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "Desktop development builds require macOS, Linux or Windows.",
    ))
  }
}

#[cfg(unix)]
type LeaseInput = Box<dyn tokio::io::AsyncRead + Send + Unpin>;

#[cfg(unix)]
fn lease_pipe() -> io::Result<LeaseInput> {
  use std::os::fd::AsFd as _;
  let descriptor = io::stdin().as_fd().try_clone_to_owned()?;
  let metadata = rustix::fs::fstat(&descriptor)?;
  match rustix::fs::FileType::from_raw_mode(metadata.st_mode) {
    rustix::fs::FileType::Fifo => Ok(Box::new(tokio::net::unix::pipe::Receiver::from_owned_fd(
      descriptor,
    )?)),
    rustix::fs::FileType::Socket => {
      // Node's child_process uses a socketpair for piped stdio on macOS.
      let stream = std::os::unix::net::UnixStream::from(descriptor);
      stream.set_nonblocking(true)?;
      Ok(Box::new(tokio::net::UnixStream::from_std(stream)?))
    }
    _ => Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "development lease requires a readable stdin pipe or Unix socket",
    )),
  }
}

#[cfg(unix)]
async fn read_lease_input(mut input: LeaseInput) -> io::Result<()> {
  use tokio::io::AsyncReadExt as _;
  if input.read(&mut [0]).await? == 0 {
    Ok(())
  } else {
    Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "development lease input must remain empty",
    ))
  }
}

#[cfg(windows)]
type LeaseInput = tokio::sync::oneshot::Receiver<io::Result<()>>;

#[cfg(windows)]
fn lease_pipe() -> io::Result<LeaseInput> {
  use std::io::{IsTerminal as _, Read as _};
  use std::os::windows::io::AsHandle as _;
  if io::stdin().is_terminal() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "development lease requires redirected stdin",
    ));
  }
  let handle = io::stdin().as_handle().try_clone_to_owned()?;
  let (tx, rx) = tokio::sync::oneshot::channel();
  std::thread::Builder::new()
    .name("ctlup-lease-input".into())
    .spawn(move || {
      let result = std::fs::File::from(handle)
        .read(&mut [0])
        .and_then(|count| {
          if count == 0 {
            Ok(())
          } else {
            Err(io::Error::new(
              io::ErrorKind::InvalidInput,
              "development lease input must remain empty",
            ))
          }
        });
      let _ = tx.send(result);
    })?;
  Ok(rx)
}

#[cfg(windows)]
async fn read_lease_input(input: LeaseInput) -> io::Result<()> {
  input.await.map_err(io::Error::other)?
}

fn cargo_arguments(options: &DevelopmentArguments) -> io::Result<Vec<String>> {
  if let Some(json) = &options.cargo_args_json {
    return serde_json::from_str(json)
      .map_err(|_| io::Error::other("--cargo-args-json must contain a JSON array of strings"));
  }
  let mut arguments = Vec::new();
  if options.release {
    arguments.push("--release".into());
  }
  for (flag, value) in [
    ("--profile", options.profile.as_deref()),
    ("--target", options.target.as_deref()),
  ] {
    if let Some(value) = value {
      arguments.extend([flag.into(), value.into()]);
    }
  }
  if let Some(directory) = &options.target_dir {
    arguments.extend([
      "--target-dir".into(),
      directory
        .to_str()
        .ok_or_else(|| io::Error::other("development output path must be UTF-8"))?
        .into(),
    ]);
  }
  Ok(arguments)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn desktop_cargo_arguments_remain_structured_and_invalid_json_is_rejected() {
    let mut options = DevelopmentArguments {
      workspace: None,
      app: false,
      ctream: false,
      ctream_agent_targets: vec![],
      destination: crate::updater::arguments::DestinationArguments::default(),
      release: false,
      profile: None,
      target_dir: None,
      target: None,
      cargo_args_json: Some(
        r#"["run","--target-dir","output with spaces","--","app-only"]"#.into(),
      ),
      lease: false,
      json: true,
    };
    assert_eq!(
      cargo_arguments(&options).unwrap(),
      [
        "run",
        "--target-dir",
        "output with spaces",
        "--",
        "app-only"
      ]
    );
    options.cargo_args_json = Some(r#"["run",42]"#.into());
    assert!(cargo_arguments(&options).is_err());
  }
}
