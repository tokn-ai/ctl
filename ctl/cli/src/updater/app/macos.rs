use ctl_client::remote_bundle::desktop::DesktopSigning;
use flate2::read::MultiGzDecoder;
use libproc::proc_pid::pidpath;
use libproc::processes::{ProcFilter, pids_by_type};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read as _, Seek as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub(super) const APP_NAME: &str = "ctmux.app";
const BUNDLE_IDENTIFIER: &str = "dev.tokn-ai.ctl.ctmux";
const EXECUTABLE_NAME: &str = "ctmux-app";
const EXECUTABLE_PATH: &str = "Contents/MacOS/ctmux-app";
const MAX_EXPANDED_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 8192;

pub(super) fn installed_version(app: &Path) -> io::Result<String> {
  let contents = app.join("Contents");
  let info = contents.join("Info.plist");
  if !fs::symlink_metadata(app)?.is_dir()
    || !fs::symlink_metadata(&contents)?.is_dir()
    || !fs::symlink_metadata(&info)?.is_file()
    || plist_string(&info, "CFBundleIdentifier")? != BUNDLE_IDENTIFIER
    || plist_string(&info, "CFBundleExecutable")? != EXECUTABLE_NAME
  {
    return Err(invalid("destination is not a ctmux application bundle"));
  }
  plist_string(&info, "CFBundleShortVersionString")
}

pub(super) fn verify_existing(
  app: &Path,
  version: &str,
  signing: DesktopSigning,
) -> io::Result<()> {
  if installed_version(app)? != version {
    return Err(invalid("application version does not match the release"));
  }
  let executable = fs::symlink_metadata(app.join(EXECUTABLE_PATH))?;
  if !executable.is_file() || executable.permissions().mode() & 0o111 == 0 {
    return Err(invalid(
      "ctmux application executable is missing or invalid",
    ));
  }
  match signing {
    DesktopSigning::Unsigned => return Ok(()),
    DesktopSigning::Signed => {}
    DesktopSigning::NotApplicable => return Err(invalid("missing macOS app signing mode")),
  }
  let requirement = format!(
    "=anchor apple generic and identifier \"{BUNDLE_IDENTIFIER}\" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
  );
  let architecture = match std::env::consts::ARCH {
    "aarch64" => "arm64",
    "x86_64" => "x86_64",
    _ => return Err(invalid("unsupported macOS architecture")),
  };
  require_success(
    Command::new("/usr/bin/codesign")
      .args(["--verify", "--deep", "--strict", "-R"])
      .arg(requirement)
      .arg(app),
    "Developer ID signature verification",
  )?;
  require_success(
    Command::new("/usr/bin/codesign")
      .args(["--verify", "--architecture", architecture])
      .arg(app),
    "native architecture verification",
  )?;
  let assessment = require_success(
    Command::new("/usr/sbin/spctl")
      .args(["--assess", "--type", "execute", "--verbose=2"])
      .arg(app),
    "macOS notarization assessment",
  )?;
  if !String::from_utf8_lossy(&assessment.stderr)
    .lines()
    .any(|line| line == "source=Notarized Developer ID")
  {
    return Err(invalid(
      "Gatekeeper did not confirm a notarized Developer ID application",
    ));
  }
  Ok(())
}

pub(super) fn stage(
  archive: &Path,
  staging: &Path,
  version: &str,
  signing: DesktopSigning,
) -> io::Result<PathBuf> {
  let app = unpack(archive, staging)?;
  verify_existing(&app, version, signing)?;
  Ok(app)
}

pub(super) fn is_running(app: &Path) -> io::Result<bool> {
  let executable = match app.join(EXECUTABLE_PATH).canonicalize() {
    Ok(path) => path,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
    Err(error) => return Err(error),
  };
  let installed = fs::metadata(&executable)?;
  for pid in pids_by_type(ProcFilter::All)? {
    let Ok(pid) = i32::try_from(pid) else {
      continue;
    };
    // A process can exit or be inaccessible while we inspect the snapshot.
    let Ok(path) = pidpath(pid) else {
      continue;
    };
    let path = Path::new(&path);
    if path == executable {
      return Ok(true);
    }
    if path
      .file_name()
      .is_some_and(|name| name.eq_ignore_ascii_case(EXECUTABLE_NAME))
      && fs::metadata(path)
        .is_ok_and(|running| installed.dev() == running.dev() && installed.ino() == running.ino())
    {
      return Ok(true);
    }
  }
  Ok(false)
}

fn unpack(archive: &Path, staging: &Path) -> io::Result<PathBuf> {
  let app = staging.join(APP_NAME);
  if exists(&app)? || exists(&staging.join("._ctmux.app"))? {
    return Err(io::Error::new(
      io::ErrorKind::AlreadyExists,
      "desktop staging path is occupied",
    ));
  }
  let mut file = File::open(archive)?;
  let paths = inspect_archive(&mut file)?;
  file.rewind()?;
  // Native tar restores AppleDouble metadata and xattrs. The archive is
  // inspected first, and extraction uses a private directory and file handle.
  require_success(
    Command::new("/usr/bin/tar")
      .args([
        "-xzf",
        "-",
        "--no-same-owner",
        "--no-same-permissions",
        "--no-fflags",
        "--no-acls",
        "--mac-metadata",
        "--xattrs",
        "-C",
      ])
      .arg(staging)
      .env_remove("COPYFILE_DISABLE")
      .stdin(Stdio::from(file)),
    "ctmux application extraction",
  )?;
  let root = app.canonicalize()?;
  let mut identities = HashSet::new();
  for path in paths {
    // Native tar consumes AppleDouble sidecars as metadata on the target.
    if path
      .file_name()
      .is_some_and(|name| name.as_encoded_bytes().starts_with(b"._"))
    {
      continue;
    }
    let extracted = staging.join(path);
    let metadata = fs::symlink_metadata(&extracted)?;
    if !identities.insert((metadata.dev(), metadata.ino())) {
      return Err(invalid(
        "application archive has paths that alias on this filesystem",
      ));
    }
    if metadata.is_symlink() && !extracted.canonicalize()?.starts_with(&root) {
      return Err(invalid("application archive contains an external symlink"));
    }
  }
  Ok(app)
}

fn inspect_archive(file: &mut File) -> io::Result<HashSet<PathBuf>> {
  let mut archive = tar::Archive::new(MultiGzDecoder::new(file).take(MAX_EXPANDED_BYTES + 1));
  let mut seen = HashSet::new();
  let mut root_found = false;
  for entry in archive.entries()? {
    let mut entry = entry?;
    validate_pax(&mut entry)?;
    let path = entry.path()?.into_owned();
    let kind = entry.header().entry_type();
    if seen.len() == MAX_ENTRIES || !seen.insert(path.clone()) {
      return Err(invalid(
        "application archive has duplicate or excessive paths",
      ));
    }
    // macOS tar can put the bundle directory's AppleDouble record alongside
    // the bundle. Its extraction only applies metadata to that directory.
    if path == Path::new("._ctmux.app") && kind.is_file() {
      continue;
    }
    let mut components = path.components();
    if components.next() != Some(Component::Normal(APP_NAME.as_ref()))
      || !components.all(|part| matches!(part, Component::Normal(_)))
    {
      return Err(invalid("application archive has a path outside ctmux.app"));
    }
    if path == Path::new(APP_NAME) {
      if !kind.is_dir() {
        return Err(invalid("ctmux.app archive root must be a directory"));
      }
      root_found = true;
    }
    if kind.is_symlink() {
      let target = entry
        .link_name()?
        .ok_or_else(|| invalid("missing application symlink target"))?;
      if !link_stays_inside(&path, &target) {
        return Err(invalid("application archive contains an external symlink"));
      }
    } else if !kind.is_file() && !kind.is_dir() {
      return Err(invalid("application archive contains a special file"));
    }
  }
  if !root_found {
    return Err(invalid("application archive is missing ctmux.app"));
  }
  // Read the gzip trailer as well. Ignore normal tar padding and reject
  // additional payloads that could be interpreted differently by extractors.
  let mut reader = archive.into_inner();
  let mut buffer = [0_u8; 8192];
  loop {
    let count = reader.read(&mut buffer)?;
    if count == 0 {
      break;
    }
    if buffer[..count].iter().any(|byte| *byte != 0) {
      return Err(invalid(
        "unexpected data after application archive terminator",
      ));
    }
  }
  if reader.limit() == 0 {
    return Err(invalid(
      "application archive exceeds its expanded size limit",
    ));
  }
  Ok(seen)
}

fn validate_pax<R: io::Read>(entry: &mut tar::Entry<'_, R>) -> io::Result<()> {
  // Ensure the two tar readers agree on which path, target and size were
  // declared, including when PAX extends the base tar header.
  let mut seen = HashSet::new();
  if let Some(fields) = entry.pax_extensions()? {
    for field in fields {
      let field = field?;
      let key = field.key_bytes();
      if key.starts_with(b"GNU.sparse.")
        || (matches!(key, b"path" | b"linkpath" | b"size") && !seen.insert(key.to_vec()))
      {
        return Err(invalid("application archive has ambiguous tar extensions"));
      }
    }
  }
  Ok(())
}

fn link_stays_inside(path: &Path, target: &Path) -> bool {
  if target.as_os_str().is_empty() {
    return false;
  }
  let mut depth = path.components().count() - 1;
  for part in target.components() {
    match part {
      Component::Normal(_) => depth += 1,
      Component::CurDir => {}
      Component::ParentDir if depth > 1 => depth -= 1,
      _ => return false,
    }
  }
  true
}

fn exists(path: &Path) -> io::Result<bool> {
  match fs::symlink_metadata(path) {
    Ok(_) => Ok(true),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error),
  }
}

fn plist_string(plist: &Path, key: &str) -> io::Result<String> {
  let output = require_success(
    Command::new("/usr/bin/plutil")
      .args([
        "-extract", key, "raw", "-expect", "string", "-n", "-o", "-", "--",
      ])
      .arg(plist),
    &format!("reading application {key}"),
  )?;
  String::from_utf8(output.stdout).map_err(io::Error::other)
}

fn require_success(command: &mut Command, operation: &str) -> io::Result<Output> {
  let output = command.env("LC_ALL", "C").output()?;
  if !output.status.success() {
    let diagnostic = if output.stderr.is_empty() {
      &output.stdout
    } else {
      &output.stderr
    };
    return Err(invalid(format!(
      "{operation} failed: {}",
      String::from_utf8_lossy(diagnostic).trim()
    )));
  }
  Ok(output)
}

fn invalid(message: impl Into<String>) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests;
