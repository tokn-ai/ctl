use super::arguments::DevelopmentArguments;
use std::io::{self, Write as _};
use std::path::Path;

pub(super) async fn run(home: &Path, options: DevelopmentArguments) -> io::Result<()> {
  let workspace = options
    .workspace
    .clone()
    .map_or_else(std::env::current_dir, Ok)?;
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

type LeaseInput = Box<dyn tokio::io::AsyncRead + Send + Unpin>;

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
