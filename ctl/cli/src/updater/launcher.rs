use ctl_core::installations::Installations;
use std::{env, io, process::Command};

pub(super) fn dispatch() -> io::Result<Option<i32>> {
  let executable = env::current_exe()?.canonicalize()?;
  let Some(name) = executable.file_stem().and_then(std::ffi::OsStr::to_str) else {
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
    (
      bundle.directory.join(
        ctl_core::bundle_layout::executable_name(&bundle.manifest.target_triple, &name).as_ref(),
      ),
      lease,
    )
  };
  let status = Command::new(path).args(env::args_os().skip(1)).status()?;
  Ok(Some(status.code().unwrap_or(1)))
}
