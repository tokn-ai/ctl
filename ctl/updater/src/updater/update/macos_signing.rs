use super::SigningRequest;
use crate::updater::sign::{self, SigningMode};
use ctl_client::component_update::{
  MacosRuntimeHelper, MacosRuntimeHelperMode, Package, PreparedUpdate, UpdateResult,
  authorize_macos_ad_hoc, install_prepared_local, macos_runtime_helper_mode, replace_macos_helper,
};
use ctl_client::installation::RuntimeSource;
use ctl_core::bundles::Store;
use ctl_core::{installations::Installations, private_fs};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

const POLICY_FILENAME: &str = "runtime-signing.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
  bundle_id: String,
  mode: SigningMode,
}

pub(super) fn recorded_mode(
  home: &Path,
  tracked: &RuntimeSource,
) -> io::Result<Option<SigningMode>> {
  let Some(active) = tracked.active.as_deref() else {
    return Ok(None);
  };
  let installations = Installations::new(home);
  let state = installations.updater_root().join("state");
  match private_fs::options()
    .read(true)
    .open(state.join(POLICY_FILENAME))
  {
    Ok(file) => {
      if !private_fs::is_private_file(&file.metadata()?) {
        return Err(io::Error::other(
          "runtime signing preference is not a private file",
        ));
      }
      let policy: Policy = serde_json::from_reader(file.take(1024)).map_err(io::Error::other)?;
      if policy.bundle_id == active {
        return Ok(Some(policy.mode));
      }
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
    Err(error) => return Err(error),
  }
  let bundle = match installations.get(active) {
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      let target = tracked
        .target_triple
        .as_deref()
        .unwrap_or(ctl_core::paths::native_target());
      Store::cache(home).get(target, active)
    }
    result => result,
  };
  let bundle = match bundle {
    Ok(bundle) => bundle,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  Ok(macos_runtime_helper_mode(&bundle)?.map(|mode| match mode {
    MacosRuntimeHelperMode::DeveloperId => SigningMode::DeveloperId,
    MacosRuntimeHelperMode::Provision => SigningMode::Provision,
    MacosRuntimeHelperMode::AdHoc => SigningMode::AdHoc,
  }))
}

pub(super) async fn install(
  home: &Path,
  mut prepared: PreparedUpdate,
  request: SigningRequest,
) -> io::Result<UpdateResult> {
  let mode = request.mode.unwrap_or_default();
  if request.mode.is_some() {
    prepared.require_macos_runtime_signing();
  }
  let existing = macos_runtime_helper_mode(&prepared.bundle)?;
  match existing {
    Some(MacosRuntimeHelperMode::DeveloperId | MacosRuntimeHelperMode::Provision) => {}
    Some(MacosRuntimeHelperMode::AdHoc) if mode == SigningMode::AdHoc => {
      authorize_macos_ad_hoc(&mut prepared)?;
    }
    _ => {
      let stage = Staging::new(home)?;
      let executable = stage.0.join("ctld");
      let mut files = prepared.bundle.read_files()?;
      let bytes = files
        .remove("ctld")
        .ok_or_else(|| io::Error::other("the runtime bundle does not contain ctld"))?;
      let mut file = private_fs::options()
        .create_new(true)
        .write(true)
        .open(&executable)?;
      file.write_all(&bytes)?;
      private_fs::set_executable(&file)?;
      drop(file);
      let version = &prepared.bundle.manifest.components["ctld"].build.version;
      let app = stage.0.join("ctld.app");
      let team;
      let helper = if mode == SigningMode::AdHoc {
        sign::sign_ad_hoc_executable(&executable).await?;
        MacosRuntimeHelper::AdHoc {
          executable: &executable,
        }
      } else {
        team =
          sign::sign_runtime_helper(home, &executable, &app, version, mode, request.interactive)
            .await?;
        MacosRuntimeHelper::Signed {
          app: &app,
          team_identifier: &team,
          development: mode == SigningMode::Provision,
        }
      };
      replace_macos_helper(home, &mut prepared, &stage.0, helper).await?;
    }
  }
  let result = install_prepared_local(home, prepared, Package::FullBundle).await?;
  if let Err(error) = remember_mode(home, &result.bundle_id, mode) {
    eprintln!(
      "ctlup: installed successfully; could not save the runtime signing preference: {error}"
    );
  }
  Ok(result)
}

fn remember_mode(home: &Path, bundle_id: &str, mode: SigningMode) -> io::Result<()> {
  let state = Installations::new(home).updater_root().join("state");
  let temporary = state.join(format!("runtime-signing-{}", uuid::Uuid::new_v4()));
  let result = (|| {
    let mut file = private_fs::options()
      .create_new(true)
      .write(true)
      .open(&temporary)?;
    serde_json::to_writer(
      &mut file,
      &Policy {
        bundle_id: bundle_id.to_owned(),
        mode,
      },
    )
    .map_err(io::Error::other)?;
    file.sync_all()?;
    fs::rename(&temporary, state.join(POLICY_FILENAME))?;
    private_fs::sync_directory(&state)
  })();
  if result.is_err() {
    let _ = fs::remove_file(temporary);
  }
  result
}

struct Staging(PathBuf);

impl Staging {
  fn new(home: &Path) -> io::Result<Self> {
    let root = Installations::new(home).updater_root().join("staging");
    let anchor = ctl_core::paths::managed_storage_anchor(&root)?;
    let mut directories = root
      .ancestors()
      .take_while(|path| *path != anchor)
      .collect::<Vec<_>>();
    directories.push(anchor);
    for directory in directories.into_iter().rev() {
      match private_fs::create_directory(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
      }
      if !private_fs::is_directory(&fs::symlink_metadata(directory)?) {
        return Err(io::Error::other("unsafe runtime signing directory"));
      }
    }
    let path = root.join(format!("runtime-signing-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&path)?;
    Ok(Self(path))
  }
}

impl Drop for Staging {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}
