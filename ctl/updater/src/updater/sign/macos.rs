use super::{SignArguments, helper, input};
use ctl_client::remote_bundle::desktop::{self, DesktopRelease, DesktopSigning, DesktopTarget};
use ctl_core::installations::Installations;
use ctl_core::private_fs;
use rustix::process::{Pid, Signal, kill_process_group};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::{self, File};
use std::future::Future;
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use tokio::process::Command;
use tokio::sync::watch;

const APP_IDENTIFIER: &str = "dev.tokn-ai.ctl.ctmux";
const APP_NAME: &str = "ctmux.app";
const SIDECARS: &[&str] = &["ctmuxd", "ctl-taskd"];

pub(super) struct Runner(watch::Receiver<bool>);

impl Runner {
  fn check(&self) -> io::Result<()> {
    if *self.0.borrow() {
      Err(interrupted())
    } else {
      Ok(())
    }
  }

  async fn cancelled(&self) {
    let _ = self.0.clone().wait_for(|stop| *stop).await;
  }

  async fn cancellable<T>(&self, operation: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    self.check()?;
    tokio::select! {
      result = operation => result,
      () = self.cancelled() => Err(interrupted()),
    }
  }

  pub(super) async fn output(&self, command: &mut Command) -> io::Result<Output> {
    self.check()?;
    let program = command
      .as_std()
      .get_program()
      .to_string_lossy()
      .into_owned();
    let child = command
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .process_group(0)
      .kill_on_drop(true)
      .spawn()?;
    let group = child
      .id()
      .and_then(|id| i32::try_from(id).ok())
      .and_then(Pid::from_raw)
      .ok_or_else(|| io::Error::other("Could not identify the signing process group."))?;
    let output = child.wait_with_output();
    tokio::pin!(output);
    let output = tokio::select! {
      result = &mut output => result?,
      () = self.cancelled() => {
        let _ = kill_process_group(group, Signal::KILL);
        let _ = output.await;
        return Err(interrupted());
      }
    };
    self.check()?;
    if !output.status.success() {
      let diagnostic = String::from_utf8_lossy(&output.stderr);
      return Err(io::Error::other(format!(
        "{program} exited with {}\n{}",
        output.status,
        diagnostic.trim()
      )));
    }
    Ok(output)
  }
}

fn interrupted() -> io::Error {
  io::Error::new(io::ErrorKind::Interrupted, "macOS signing was cancelled.")
}

struct SigningCredentials {
  profile: PathBuf,
  key: PathBuf,
  key_id: OsString,
  issuer: OsString,
}

impl SigningCredentials {
  fn load() -> io::Result<Self> {
    let profile = credential_file("CTLD_PROVISIONING_PROFILE")?;
    let key = credential_file("APPLE_API_KEY_PATH")?;
    Ok(Self {
      profile,
      key,
      key_id: required_env("APPLE_API_KEY")?,
      issuer: required_env("APPLE_API_ISSUER")?,
    })
  }
}

fn required_env(name: &str) -> io::Result<OsString> {
  std::env::var_os(name)
    .filter(|value| !value.is_empty())
    .ok_or_else(|| io::Error::other(format!("Set {name} to sign a macOS release.")))
}

fn credential_file(name: &str) -> io::Result<PathBuf> {
  let path = std::path::absolute(PathBuf::from(required_env(name)?))?;
  let metadata = fs::symlink_metadata(&path)?;
  if !metadata.is_file() || metadata.len() == 0 {
    return Err(io::Error::other(format!(
      "{name} must name a nonempty regular file."
    )));
  }
  Ok(path)
}

struct Scratch {
  stage: PathBuf,
  output: PathBuf,
  complete: bool,
}

impl Scratch {
  fn allocate(output: &Path) -> io::Result<Self> {
    let output = std::path::absolute(output)?;
    let parent = output
      .parent()
      .ok_or_else(|| io::Error::other("Choose an output directory."))?;
    fs::DirBuilder::new()
      .recursive(true)
      .mode(0o700)
      .create(parent)?;
    private_fs::create_directory(&output).map_err(|error| {
      if error.kind() == io::ErrorKind::AlreadyExists {
        io::Error::new(
          error.kind(),
          format!("Signing output already exists at {}.", output.display()),
        )
      } else {
        error
      }
    })?;
    let stage = parent.join(format!(".ctmux-signing-{}", uuid::Uuid::new_v4().simple()));
    if let Err(error) = private_fs::create_directory(&stage) {
      let _ = fs::remove_dir(&output);
      return Err(error);
    }
    Ok(Self {
      stage,
      output,
      complete: false,
    })
  }

  fn finish(&mut self, assets: &Path) -> io::Result<()> {
    fs::rename(assets, &self.output)?;
    self.complete = true;
    Ok(())
  }
}

impl Drop for Scratch {
  fn drop(&mut self) {
    if let Err(error) = fs::remove_dir_all(&self.stage) {
      eprintln!("ctlup: could not remove signing staging directory: {error}");
    }
    if !self.complete
      && let Err(error) = fs::remove_dir(&self.output)
    {
      eprintln!("ctlup: could not remove incomplete signing output directory: {error}");
    }
  }
}

#[derive(Serialize)]
struct Manifest {
  schema_version: u16,
  target: String,
  bundle_id: String,
  git_revision: String,
  signing_mode: &'static str,
  #[serde(skip_serializing_if = "Option::is_none")]
  installation_layout_version: Option<u16>,
  assets: [Asset; 2],
}

#[derive(Serialize)]
struct Asset {
  name: String,
  sha256: String,
}

fn record_asset(directory: &Path, name: String) -> io::Result<Asset> {
  let (size, digest) = ctl_core::bundles::sha256_reader(File::open(directory.join(&name))?)?;
  if size == 0 {
    return Err(io::Error::other("macOS signing produced an empty package."));
  }
  let sha256 = hex::encode(digest);
  fs::write(
    directory.join(format!("{name}.sha256")),
    format!("{sha256}  {name}\n"),
  )?;
  Ok(Asset { name, sha256 })
}

async fn sign_app(runner: &Runner, app: &Path, identity: &str) -> io::Result<()> {
  let sidecars = SIDECARS
    .iter()
    .map(|name| app.join("Contents/MacOS").join(name));
  for item in sidecars.chain(std::iter::once(app.to_owned())) {
    runner
      .output(
        Command::new("/usr/bin/codesign")
          .args([
            "--force",
            "--options",
            "runtime",
            "--timestamp",
            "--sign",
            identity,
          ])
          .arg(item),
      )
      .await?;
  }
  Ok(())
}

#[derive(Deserialize)]
struct Notarization {
  status: String,
  id: Option<String>,
}

async fn notarize(
  runner: &Runner,
  submission: &Path,
  product: &Path,
  credentials: &SigningCredentials,
) -> io::Result<()> {
  eprintln!(
    "Submitting {} to Apple for notarization…",
    product.file_name().unwrap_or_default().to_string_lossy()
  );
  let output = runner
    .output(
      Command::new("/usr/bin/xcrun")
        .args(["notarytool", "submit"])
        .arg(submission)
        .arg("--key")
        .arg(&credentials.key)
        .arg("--key-id")
        .arg(&credentials.key_id)
        .arg("--issuer")
        .arg(&credentials.issuer)
        .args(["--wait", "--output-format", "json"]),
    )
    .await?;
  let response: Notarization = serde_json::from_slice(&output.stdout).map_err(io::Error::other)?;
  if response.status != "Accepted" {
    let id = response
      .id
      .map_or_else(String::new, |id| format!(" (submission {id})"));
    return Err(io::Error::other(format!(
      "Apple did not accept notarization{id}."
    )));
  }
  runner
    .output(
      Command::new("/usr/bin/xcrun")
        .args(["stapler", "staple"])
        .arg(product),
    )
    .await?;
  validate_ticket(runner, product).await
}

async fn validate_ticket(runner: &Runner, product: &Path) -> io::Result<()> {
  runner
    .output(
      Command::new("/usr/bin/xcrun")
        .args(["stapler", "validate"])
        .arg(product),
    )
    .await?;
  Ok(())
}

async fn verify_app(runner: &Runner, app: &Path, arch: &str) -> io::Result<()> {
  let requirement = format!(
    "=anchor apple generic and identifier \"{APP_IDENTIFIER}\" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
  );
  runner
    .output(
      Command::new("/usr/bin/codesign")
        .args([
          "--verify",
          "--deep",
          "--strict",
          "--test-requirement",
          &requirement,
        ])
        .arg(app),
    )
    .await?;
  runner
    .output(
      Command::new("/usr/bin/codesign")
        .args(["--verify", "--architecture", arch])
        .arg(app),
    )
    .await?;
  let assessment = runner
    .output(
      Command::new("/usr/sbin/spctl")
        .args(["--assess", "--type", "execute", "--verbose=2"])
        .arg(app),
    )
    .await?;
  if !String::from_utf8_lossy(&assessment.stderr)
    .lines()
    .any(|line| line == "source=Notarized Developer ID")
  {
    return Err(io::Error::other(
      "Gatekeeper did not confirm a notarized Developer ID application.",
    ));
  }
  Ok(())
}

async fn pack_image(
  runner: &Runner,
  app: &Path,
  stage: &Path,
  dmg: &Path,
  identity: &str,
) -> io::Result<()> {
  let image = stage.join("image");
  private_fs::create_directory(&image)?;
  runner
    .output(
      Command::new("/usr/bin/ditto")
        .arg(app)
        .arg(image.join(APP_NAME)),
    )
    .await?;
  std::os::unix::fs::symlink("/Applications", image.join("Applications"))?;
  runner
    .output(
      Command::new("/usr/bin/hdiutil")
        .args([
          "create",
          "-volname",
          "ctmux",
          "-format",
          "UDZO",
          "-srcfolder",
        ])
        .arg(image)
        .arg(dmg),
    )
    .await?;
  runner
    .output(
      Command::new("/usr/bin/codesign")
        .args([
          "--timestamp",
          "--identifier",
          &format!("{APP_IDENTIFIER}.dmg"),
          "--sign",
          identity,
        ])
        .arg(dmg),
    )
    .await?;
  Ok(())
}

async fn stage_signed_app(
  runner: &Runner,
  release: &DesktopRelease,
  credentials: &SigningCredentials,
  stage: &Path,
  arch: &str,
) -> io::Result<(PathBuf, String)> {
  let unpacked = stage.join("bundle");
  private_fs::create_directory(&unpacked)?;
  let archive = stage.join(release.asset_name());
  eprintln!("Downloading {}…", release.asset_name());
  runner.cancellable(release.download(&archive)).await?;
  let app = tokio::task::spawn_blocking({
    let version = release.app_version.clone();
    move || input::prepare(&archive, &unpacked, &version)
  })
  .await
  .map_err(io::Error::other)??;
  for executable in [
    "Contents/MacOS/ctmux-app",
    "Contents/MacOS/ctmuxd",
    "Contents/MacOS/ctl-taskd",
    "Contents/Helpers/ctld.app/Contents/MacOS/ctld",
  ] {
    runner
      .output(
        Command::new("/usr/bin/lipo")
          .arg(app.join(executable))
          .args(["-verify_arch", arch]),
      )
      .await?;
  }
  let identity = helper::sign(
    &app,
    stage,
    &release.app_version,
    &credentials.profile,
    runner,
  )
  .await?;
  sign_app(runner, &app, &identity).await?;
  let submission = stage.join("ctmux.zip");
  runner
    .output(
      Command::new("/usr/bin/ditto")
        .args(["-c", "-k", "--keepParent"])
        .arg(&app)
        .arg(&submission),
    )
    .await?;
  notarize(runner, &submission, &app, credentials).await?;
  verify_app(runner, &app, arch).await?;
  fs::remove_file(submission)?;
  Ok((app, identity))
}

async fn pack_archive(
  runner: &Runner,
  app: &Path,
  stage: &Path,
  archive: &Path,
  version: &str,
  arch: &str,
) -> io::Result<()> {
  runner
    .output(
      Command::new("/usr/bin/tar")
        .arg("-czf")
        .arg(archive)
        .arg("--mac-metadata")
        .arg("-C")
        .arg(app.parent().expect("staged app has a parent"))
        .arg(APP_NAME)
        .env_remove("COPYFILE_DISABLE")
        .env_remove("TAR_OPTIONS"),
    )
    .await?;
  let transported = stage.join("transported");
  private_fs::create_directory(&transported)?;
  let transported_app = tokio::task::spawn_blocking({
    let version = version.to_owned();
    let archive = archive.to_owned();
    move || {
      crate::updater::app::macos::stage(&archive, &transported, &version, DesktopSigning::Unsigned)
    }
  })
  .await
  .map_err(io::Error::other)??;
  validate_ticket(runner, &transported_app).await?;
  verify_app(runner, &transported_app, arch).await
}

async fn finish_signing(
  runner: &Runner,
  home: &Path,
  version: &str,
  target: DesktopTarget,
  credentials: &SigningCredentials,
  json: bool,
) -> io::Result<()> {
  let release = runner
    .cancellable(desktop::unsigned_macos_release(version, target))
    .await?;
  let arch = if target == DesktopTarget::MacArm64 {
    "arm64"
  } else {
    "x86_64"
  };
  let target = target.triple();
  let output = Installations::new(home)
    .updater_root()
    .join("signed")
    .join(format!("v{version}"))
    .join(target);
  let mut scratch = Scratch::allocate(&output)?;
  let (app, identity) =
    stage_signed_app(runner, &release, credentials, &scratch.stage, arch).await?;
  let assets = scratch.stage.join("assets");
  private_fs::create_directory(&assets)?;
  let prefix = format!("ctmux-{version}-{target}");
  let dmg_name = format!("{prefix}.dmg");
  let dmg = assets.join(&dmg_name);
  pack_image(runner, &app, &scratch.stage, &dmg, &identity).await?;
  notarize(runner, &dmg, &dmg, credentials).await?;
  runner
    .output(
      Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(&dmg),
    )
    .await?;

  let archive_name = format!("{prefix}.app.tar.gz");
  pack_archive(
    runner,
    &app,
    &scratch.stage,
    &assets.join(&archive_name),
    version,
    arch,
  )
  .await?;

  let manifest = tokio::task::spawn_blocking({
    let assets = assets.clone();
    let target = target.to_owned();
    let installation_layout_version = release.installation_layout_version();
    let bundle_id = release.bundle_id;
    let git_revision = release.source_revision;
    move || {
      let manifest = Manifest {
        schema_version: 1,
        target,
        bundle_id,
        git_revision,
        signing_mode: "signed",
        installation_layout_version,
        assets: [
          record_asset(&assets, dmg_name)?,
          record_asset(&assets, archive_name)?,
        ],
      };
      let mut json = serde_json::to_string_pretty(&manifest).map_err(io::Error::other)?;
      json.push('\n');
      fs::write(
        assets.join(format!("desktop-{}.json", manifest.target)),
        json,
      )?;
      Ok::<_, io::Error>(manifest)
    }
  })
  .await
  .map_err(io::Error::other)??;
  runner.check()?;
  scratch.finish(&assets)?;
  if json {
    crate::updater::print_json(&serde_json::json!({
      "output": scratch.output.display().to_string(),
      "manifest": manifest,
    }))?;
  } else {
    println!(
      "Signed and notarized macOS packages written to {}",
      scratch.output.display()
    );
  }
  Ok(())
}

async fn renew_development(home: &Path, workspace: Option<PathBuf>, json: bool) -> io::Result<()> {
  let workspace = workspace.map_or_else(std::env::current_dir, Ok)?;
  let prepared = ctl_client::development::renew_signing(home, &workspace).await?;
  let result = &prepared.result;
  if json {
    crate::updater::print_json(result)?;
  } else {
    println!(
      "Re-signed ctld for development build {} ({}). Restart ctld to use the renewed helper.",
      result.bundle_id, result.target_triple
    );
  }
  Ok(())
}

async fn sign(runner: &Runner, home: &Path, options: SignArguments) -> io::Result<()> {
  let Some(version) = options.version else {
    return runner
      .cancellable(renew_development(home, options.workspace, options.json))
      .await;
  };
  let version = version.strip_prefix('v').unwrap_or(&version);
  desktop::validate_macos_release_version(version)?;
  let target = match options.target.as_deref() {
    Some("aarch64-apple-darwin") => DesktopTarget::MacArm64,
    Some("x86_64-apple-darwin") => DesktopTarget::MacX64,
    None => DesktopTarget::native()?,
    _ => {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "Choose an Apple Silicon or Intel macOS target.",
      ));
    }
  };
  let credentials = SigningCredentials::load()?;
  finish_signing(runner, home, version, target, &credentials, options.json).await
}

pub(super) async fn run(home: &Path, options: SignArguments) -> io::Result<()> {
  use tokio::signal::unix::{SignalKind, signal};

  let mut interrupt = signal(SignalKind::interrupt())?;
  let mut terminate = signal(SignalKind::terminate())?;
  let (stop, cancelled) = watch::channel(false);
  let runner = Runner(cancelled);
  let signing = sign(&runner, home, options);
  tokio::pin!(signing);
  tokio::select! {
    result = &mut signing => return result,
    _ = interrupt.recv() => {},
    _ = terminate.recv() => {},
  }
  let _ = stop.send(true);
  signing.await
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;

  fn scratch() -> Scratch {
    let output = Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("../../target")
      .join(format!("test-ctlup-sign-{}", uuid::Uuid::new_v4()));
    Scratch::allocate(&output).unwrap()
  }

  #[test]
  fn signing_exposes_assets_only_after_completion() {
    let outer = scratch();
    let output = outer.stage.join("signed");
    let stage = {
      let scratch = Scratch::allocate(&output).unwrap();
      fs::write(scratch.stage.join("partial.dmg"), b"partial disk image").unwrap();
      assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
      assert_eq!(
        Scratch::allocate(&output).err().unwrap().kind(),
        io::ErrorKind::AlreadyExists
      );
      scratch.stage.clone()
    };
    assert!(!stage.exists());
    assert!(!output.exists());

    let stage = {
      let mut scratch = Scratch::allocate(&output).unwrap();
      let assets = scratch.stage.join("assets");
      fs::create_dir(&assets).unwrap();
      let name = "example.dmg";
      fs::write(assets.join(name), b"disk image").unwrap();
      let asset = record_asset(&assets, name.into()).unwrap();
      scratch.finish(&assets).unwrap();
      assert_eq!(
        fs::read_to_string(output.join("example.dmg.sha256")).unwrap(),
        format!("{}  {name}\n", asset.sha256)
      );
      scratch.stage.clone()
    };
    assert!(!stage.exists());
    assert_eq!(fs::read(output.join("example.dmg")).unwrap(), b"disk image");
  }

  #[tokio::test]
  async fn cancellation_reaps_the_signing_process() {
    let scratch = scratch();
    let pid_file = scratch.stage.join("worker.pid");
    let (stop, cancelled) = watch::channel(false);
    let runner = Runner(cancelled);
    let mut command = Command::new("/bin/sh");
    command
      .args([
        "-c",
        "printf '%s\\n' \"$$\" > \"$1\"; exec /bin/sleep 15",
        "sh",
      ])
      .arg(&pid_file);
    let (result, pid) = tokio::join!(runner.output(&mut command), async {
      let pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
          if let Ok(contents) = fs::read_to_string(&pid_file)
            && let Ok(raw) = contents.trim().parse()
          {
            break Pid::from_raw(raw).unwrap();
          }
          tokio::time::sleep(Duration::from_millis(10)).await;
        }
      })
      .await;
      stop.send(true).unwrap();
      pid
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert_eq!(
      rustix::process::test_kill_process(pid.unwrap()),
      Err(rustix::io::Errno::SRCH)
    );
  }
}
