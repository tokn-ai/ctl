use ctl_core::installations::Installations;
use std::{env, io, path::Path, process::Command};

pub(super) fn dispatch() -> io::Result<Option<i32>> {
  let executable = env::current_exe()?.canonicalize()?;
  let invocation = env::args_os().next().unwrap_or_default();
  #[cfg(unix)]
  if executable.file_name().and_then(std::ffi::OsStr::to_str) != Some(".ctl-launcher") {
    return Ok(None);
  }
  let Some(name) = Path::new(&invocation)
    .file_stem()
    .and_then(std::ffi::OsStr::to_str)
  else {
    return Ok(None);
  };
  let name = name.to_ascii_lowercase();
  if name != "ctlup" && !ctl_core::bundle_layout::ALL.contains(&name.as_str()) {
    return Ok(None);
  }
  let home = dirs::home_dir().ok_or_else(|| io::Error::other("home directory is unavailable"))?;
  let installations = Installations::new(&home);
  let bin = match installations.root().join("bin").canonicalize() {
    Ok(path) => path,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  if executable.parent() != Some(bin.as_path()) {
    return Ok(None);
  }
  let (path, _lease) = if name == "ctlup" {
    installations
      .manager_and_lease()?
      .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no managed updater is installed"))?
  } else {
    let (bundle, lease) = installations
      .active_and_lease()?
      .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no active ctl installation"))?;
    let signed_helper = "ctld.app/Contents/MacOS/ctld";
    let path = if cfg!(target_os = "macos")
      && name == "ctld"
      && bundle.manifest.files.contains_key(signed_helper)
    {
      bundle.directory.join(signed_helper)
    } else {
      bundle.executable(&name).ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::NotFound,
          "the active runtime omits this command",
        )
      })?
    };
    (path, lease)
  };
  let mut arguments = env::args_os().skip(1);
  if name == "ctlup"
    && arguments.len() == 1
    && arguments.next().as_deref() == Some("updater-digest".as_ref())
  {
    super::print_updater_digest(&path)?;
    return Ok(Some(0));
  }
  let status = Command::new(path).args(env::args_os().skip(1)).status()?;
  #[cfg(unix)]
  let code = status.code().or_else(|| {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal().map(|signal| 128 + signal)
  });
  #[cfg(windows)]
  let code = status.code();
  Ok(Some(code.unwrap_or(1)))
}
