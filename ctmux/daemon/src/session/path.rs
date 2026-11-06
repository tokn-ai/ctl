use portable_pty::CommandBuilder;
use std::env;
use std::path::Path;

/// Check at each spawn so an existing daemon sees subsequently installed clients.
pub(super) fn include_managed_clients(builder: &mut CommandBuilder, current: &Path) {
  if !["ctl", "ctmux"]
    .iter()
    .all(|name| executable(&current.join(format!("{name}{}", env::consts::EXE_SUFFIX))))
  {
    return;
  }
  let inherited = builder
    .get_env("PATH")
    .into_iter()
    .flat_map(env::split_paths);
  let entries =
    std::iter::once(current.to_path_buf()).chain(inherited.filter(|entry| entry != current));
  if let Ok(path) = env::join_paths(entries) {
    builder.env("PATH", path);
  }
}

fn executable(path: &Path) -> bool {
  if !path.is_file() {
    return false;
  }
  #[cfg(unix)]
  {
    rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
  }
  #[cfg(not(unix))]
  {
    true
  }
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use std::os::unix::fs::PermissionsExt;

  #[test]
  fn every_new_child_observes_the_current_clients_and_preserves_its_search_path() {
    let current = env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir(&current).unwrap();
    let prior = current.join("custom");
    let tail = current.join("other");
    let inherited = env::join_paths([&prior, &current, &tail, &current]).unwrap();
    let mut command = CommandBuilder::new("fixture");
    command.env("PATH", &inherited);
    // A historical bundle or a partial install must not affect child commands.
    include_managed_clients(&mut command, &current);
    assert_eq!(command.get_env("PATH"), Some(inherited.as_os_str()));
    for name in ["ctl", "ctmux"] {
      std::fs::write(current.join(name), []).unwrap();
    }
    std::fs::set_permissions(current.join("ctl"), std::fs::Permissions::from_mode(0o700)).unwrap();
    include_managed_clients(&mut command, &current);
    assert_eq!(command.get_env("PATH"), Some(inherited.as_os_str()));

    std::fs::set_permissions(
      current.join("ctmux"),
      std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    include_managed_clients(&mut command, &current);
    include_managed_clients(&mut command, &current);
    assert_eq!(
      env::split_paths(command.get_env("PATH").unwrap()).collect::<Vec<_>>(),
      vec![current.clone(), prior, tail]
    );
    std::fs::remove_dir_all(current).unwrap();
  }
}
