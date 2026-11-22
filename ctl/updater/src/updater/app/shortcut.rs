use ctl_core::installations::Installations;
use ctl_core::private_fs;
use lnks::Shortcut;
use std::ffi::OsString;
use std::io::Write as _;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::{
  fs, io,
  path::{Path, PathBuf},
};

// Explorer follows the native shortcut straight into the installed GUI.
pub(super) fn create(destination: &Path, target: &Path) -> io::Result<()> {
  if !private_fs::is_owned_file(&fs::symlink_metadata(target)?) {
    return Err(io::Error::other("desktop executable is not an owned file"));
  }
  let target = shell_path(dunce::canonicalize(target)?);
  let working_dir = target
    .parent()
    .ok_or_else(|| io::Error::other("desktop executable needs a parent directory"))?;
  let id_list = shell_item(&target)?;
  let target_path = target.as_os_str().encode_wide().collect::<Vec<_>>();
  let working_dir = working_dir.as_os_str().encode_wide().collect::<Vec<_>>();
  let bytes = encode(&id_list, &target_path, &working_dir)?;
  let mut file = private_fs::options()
    .write(true)
    .create_new(true)
    .open(destination)?;
  file.write_all(&bytes)?;
  file.sync_all()
}

pub(super) fn managed(
  path: &Path,
  store: &Path,
  installations: &Installations,
) -> io::Result<bool> {
  let shortcut = Shortcut::load(path).map_err(io::Error::other)?;
  let Some(target) = shortcut.target_path.as_deref() else {
    return Ok(false);
  };
  if shortcut.arguments.is_some() || shortcut.working_dir.as_deref() != target.parent() {
    return Ok(false);
  }
  // Resolve the store, since the shell records a full path. The immutable
  // object itself may be missing when this link is inspected for repair.
  let store = shell_path(dunce::canonicalize(store)?);
  let Ok(relative) = target.strip_prefix(store) else {
    return Ok(false);
  };
  let mut components = relative.components();
  let Some(name) = components
    .next()
    .and_then(|component| component.as_os_str().to_str())
  else {
    return Ok(false);
  };
  Ok(
    components.as_path() == Path::new(r"ctmux\ctmux-app.exe")
      && installations.is_desktop_object_name(name)?,
  )
}

fn shell_item(target: &Path) -> io::Result<Vec<u8>> {
  const CREATE_NO_WINDOW: u32 = 0x0800_0000;
  let system_root = std::env::var_os("SystemRoot")
    .ok_or_else(|| io::Error::other("Windows system directory is unavailable"))?;
  let powershell = Path::new(&system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
  let output = Command::new(powershell)
    .creation_flags(CREATE_NO_WINDOW)
    .args([
      "-NoLogo",
      "-NoProfile",
      "-NonInteractive",
      "-STA",
      "-Command",
      "& ([ScriptBlock]::Create($env:CTLUP_SHORTCUT_SCRIPT))",
    ])
    .env("CTLUP_SHORTCUT_SCRIPT", include_str!("shortcut_pidl.ps1"))
    .env("CTLUP_SHORTCUT_TARGET", target)
    .stdin(Stdio::null())
    .output()?;
  if !output.status.success() {
    return Err(io::Error::other(format!(
      "could not identify the desktop executable in Windows Shell: {}",
      String::from_utf8_lossy(&output.stderr).trim()
    )));
  }
  Ok(output.stdout)
}

fn shell_path(path: PathBuf) -> PathBuf {
  let mut units = path.as_os_str().encode_wide().collect::<Vec<_>>();
  let extended_unc: Vec<_> = r"\\?\UNC\".encode_utf16().collect();
  if units.starts_with(&extended_unc) {
    // LinkInfo stores the network share as a conventional UNC name.
    units.drain(2..extended_unc.len());
    return OsString::from_wide(&units).into();
  }
  path
}

// Explorer identifies the executable using the native item ID list. LinkInfo retains
// its absolute filesystem path for resolution when the executable moves or is missing.
// https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-shllink/6813269d-0cc8-4be2-933f-e96e8e3412dc
fn encode(id_list: &[u8], target: &[u16], working_dir: &[u16]) -> io::Result<Vec<u8>> {
  const HEADER_SIZE: u32 = 0x4c;
  const SHELL_LINK_CLSID: [u8; 16] = [
    0x01, 0x14, 0x02, 0x00, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46,
  ];
  const HAS_ID_LIST: u32 = 0x01;
  const LINK_INFO: u32 = 0x02;
  const WORKING_DIR: u32 = 0x10;
  const UNICODE: u32 = 0x80;
  const SW_SHOWNORMAL: u32 = 1;
  const MAX_PATH: usize = 260;
  if target.len() > 32_767 || working_dir.len() > MAX_PATH {
    return Err(io::Error::other(
      "desktop path exceeds Windows shortcut limits",
    ));
  }
  let id_list_size = u16::try_from(id_list.len())
    .ok()
    .filter(|size| *size >= 4 && id_list.ends_with(&[0, 0]))
    .ok_or_else(|| io::Error::other("invalid Windows Shell item ID list"))?;
  let mut link = HEADER_SIZE.to_le_bytes().to_vec();
  link.extend_from_slice(&SHELL_LINK_CLSID);
  append_u32(&mut link, [HAS_ID_LIST | LINK_INFO | WORKING_DIR | UNICODE]);
  link.resize(0x3c, 0);
  append_u32(&mut link, [SW_SHOWNORMAL]);
  link.resize(HEADER_SIZE as usize, 0);

  link.extend_from_slice(&id_list_size.to_le_bytes());
  link.extend_from_slice(id_list);
  link.extend(link_info(target)?);
  let length = u16::try_from(working_dir.len()).expect("bounded Windows working directory");
  link.extend_from_slice(&length.to_le_bytes());
  link.extend(working_dir.iter().flat_map(|unit| unit.to_le_bytes()));
  append_u32(&mut link, [0]); // ExtraData terminal block.
  Ok(link)
}

fn link_info(target: &[u16]) -> io::Result<Vec<u8>> {
  const INFO_HEADER_SIZE: u32 = 0x24;
  const VOLUME_AND_LOCAL_BASE_PATH: u32 = 1;
  const NETWORK_LINK_AND_SUFFIX: u32 = 2;
  const SEPARATOR: u16 = b'\\' as u16;
  let mut info = vec![0; INFO_HEADER_SIZE as usize];
  let (flags, volume, local, network, suffix) = if let Some(remote) =
    target.strip_prefix(&[SEPARATOR, SEPARATOR])
    && !remote.starts_with(&[u16::from(b'?'), SEPARATOR])
  {
    let suffix = remote
      .iter()
      .enumerate()
      .filter(|(_, unit)| **unit == SEPARATOR)
      .nth(1)
      .map(|(index, _)| index + 2)
      .ok_or_else(|| io::Error::other("desktop network path needs a server share"))?;
    let (share, suffix) = target.split_at(suffix);
    let network = offset(&info);
    info.extend(network_link(share));
    (NETWORK_LINK_AND_SUFFIX, 0, 0, network, suffix)
  } else {
    let volume = offset(&info);
    // VolumeID with an unknown drive type/serial and an empty volume label.
    // The absolute base path identifies the executable directly.
    append_u32(&mut info, [17, 0, 0, 16]);
    info.push(0);
    let local = append_ansi(&mut info, target);
    (VOLUME_AND_LOCAL_BASE_PATH, volume, local, 0, &[][..])
  };
  let suffix_ansi = append_ansi(&mut info, suffix);
  let local_unicode = if local == 0 {
    0
  } else {
    append_unicode(&mut info, target)
  };
  let suffix_unicode = append_unicode(&mut info, suffix);
  let header = [
    offset(&info),
    INFO_HEADER_SIZE,
    flags,
    volume,
    local,
    network,
    suffix_ansi,
    local_unicode,
    suffix_unicode,
  ];
  write_header(&mut info, header);
  Ok(info)
}

fn network_link(share: &[u16]) -> Vec<u8> {
  let mut network = vec![0; 0x1c]; // Header including the two Unicode offsets.
  let name = append_ansi(&mut network, share);
  let name_unicode = append_unicode(&mut network, share);
  let header = [offset(&network), 0, name, 0, 0, name_unicode, 0];
  write_header(&mut network, header);
  network
}

fn offset(bytes: &[u8]) -> u32 {
  u32::try_from(bytes.len()).expect("bounded Windows path")
}

fn append_u32<const N: usize>(bytes: &mut Vec<u8>, fields: [u32; N]) {
  bytes.extend(fields.into_iter().flat_map(u32::to_le_bytes));
}

fn write_header<const N: usize>(bytes: &mut [u8], fields: [u32; N]) {
  for (value, slot) in fields.into_iter().zip(bytes.as_chunks_mut::<4>().0) {
    *slot = value.to_le_bytes();
  }
}

fn append_ansi(bytes: &mut Vec<u8>, text: &[u16]) -> u32 {
  let start = offset(bytes);
  // ASCII is shared by Windows code pages. The Unicode field stores the
  // exact spelling when the target contains other characters.
  bytes.extend(
    text
      .iter()
      .map(|&unit| u8::try_from(unit).ok().filter(u8::is_ascii).unwrap_or(b'?')),
  );
  bytes.push(0);
  start
}

fn append_unicode(bytes: &mut Vec<u8>, text: &[u16]) -> u32 {
  if !bytes.len().is_multiple_of(2) {
    bytes.push(0);
  }
  let start = offset(bytes);
  bytes.extend(text.iter().flat_map(|unit| unit.to_le_bytes()));
  bytes.extend_from_slice(&[0, 0]);
  start
}
