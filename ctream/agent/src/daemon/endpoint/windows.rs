use interprocess::{
  local_socket::{ListenerOptions, tokio::Stream},
  os::windows::{local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor},
};
use std::{
  io,
  mem::size_of_val,
  os::windows::io::{AsHandle, AsRawHandle},
  path::PathBuf,
  ptr::null_mut,
};
use widestring::{U16CStr, U16CString};
use windows_sys::Win32::{
  Foundation::{HANDLE, LocalFree},
  Security::{
    Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT},
    EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSID, TOKEN_USER, TokenUser,
  },
  System::RemoteDesktop::ProcessIdToSessionId,
};

// TOKEN_USER is followed by a variable length SID. A SID is at most 68 bytes,
// so this buffer has space for both and pointer alignment on all Windows targets.
fn with_user<T>(f: impl FnOnce(PSID) -> io::Result<T>) -> io::Result<T> {
  let mut buffer = [0usize; 32];
  let mut length = 0;
  // SAFETY: -4 is GetCurrentProcessToken's documented pseudo-handle. The
  // aligned buffer stays alive throughout the callback using its embedded SID.
  unsafe {
    if GetTokenInformation(
      (-4isize) as HANDLE,
      TokenUser,
      buffer.as_mut_ptr().cast(),
      size_of_val(&buffer) as u32,
      &mut length,
    ) == 0
    {
      return Err(io::Error::last_os_error());
    }
    f((*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid)
  }
}

pub(crate) fn host_account_id() -> io::Result<String> {
  with_user(|sid| {
    let mut text = null_mut();
    // SAFETY: sid is the live token user's SID; Windows allocates a terminated
    // string that we copy before freeing it with LocalFree.
    unsafe {
      if ConvertSidToStringSidW(sid, &mut text) == 0 {
        return Err(io::Error::last_os_error());
      }
      let result = U16CStr::from_ptr_str(text).to_string_lossy();
      LocalFree(text.cast());
      Ok(result)
    }
  })
}

fn interactive_session() -> io::Result<u32> {
  let mut session = 0;
  // SAFETY: the process ID exists and session is writable.
  if unsafe { ProcessIdToSessionId(std::process::id(), &mut session) } == 0 {
    return Err(io::Error::last_os_error());
  }
  Ok(session)
}

// Tests run distinct daemon processes concurrently under the same account and
// logon. Keep the named pipe and every lock in the same fixture namespace.
#[cfg(any(test, all(debug_assertions, feature = "test-fixtures")))]
fn fixture() -> io::Result<Option<(PathBuf, uuid::Uuid)>> {
  let Some(directory) = std::env::var_os("CTREAM_TEST_DAEMON_RUNTIME").map(PathBuf::from) else {
    return Ok(None);
  };
  let id = directory
    .file_name()
    .and_then(|stem| stem.to_str())
    .and_then(|stem| uuid::Uuid::try_parse(stem).ok())
    .filter(|_| directory.is_absolute())
    .ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::InvalidInput,
        "expected an isolated daemon fixture directory ending in a UUID",
      )
    })?;
  Ok(Some((directory, id)))
}

pub(super) fn path() -> io::Result<PathBuf> {
  #[cfg(any(test, all(debug_assertions, feature = "test-fixtures")))]
  if let Some((_, id)) = fixture()? {
    return Ok(PathBuf::from(format!(r"\\.\pipe\ctream-test-{id}")));
  }
  Ok(PathBuf::from(format!(
    r"\\.\pipe\ctream-live-v2-{}-{}",
    host_account_id()?,
    interactive_session()?
  )))
}

pub(super) fn restrict(options: ListenerOptions<'_>) -> io::Result<ListenerOptions<'_>> {
  let sid = host_account_id()?;
  let sddl =
    U16CString::from_str(format!("O:{sid}D:P(A;;GA;;;{sid})")).map_err(io::Error::other)?;
  Ok(options.security_descriptor(SecurityDescriptor::deserialize(&sddl)?))
}

pub(super) fn directory() -> io::Result<PathBuf> {
  #[cfg(any(test, all(debug_assertions, feature = "test-fixtures")))]
  if let Some((directory, _)) = fixture()? {
    return Ok(directory);
  }
  let base = dirs::data_local_dir().ok_or_else(|| io::Error::other("missing LocalAppData"))?;
  let dir = base
    .join("ctream")
    .join(format!("desktop-{}", interactive_session()?));
  std::fs::create_dir_all(&dir)?;
  use std::os::windows::fs::MetadataExt as _;
  for path in [dir.parent().unwrap(), dir.as_path()] {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_attributes() & 0x400 != 0 {
      return Err(io::Error::other("unsafe desktop state directory"));
    }
  }
  Ok(dir)
}

pub(super) fn private_file(path: &std::path::Path) -> io::Result<std::fs::File> {
  use std::{
    fs,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
  };
  let file = fs::OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .truncate(false)
    .custom_flags(0x0020_0000)
    .open(path)?;
  let info = file.metadata()?;
  if !info.is_file() || info.file_attributes() & 0x400 != 0 {
    return Err(io::Error::other("unsafe gesture fence"));
  }
  Ok(file)
}

pub(super) fn fence_file() -> io::Result<std::fs::File> {
  #[cfg(any(test, all(debug_assertions, feature = "test-fixtures")))]
  if let Some((directory, _)) = fixture()? {
    return private_file(&directory.join("gestures.lock"));
  }
  // Keep the fence shared with hosts from before online upgrades.
  let base = dirs::data_local_dir().ok_or_else(|| io::Error::other("missing LocalAppData"))?;
  std::fs::create_dir_all(&base)?;
  private_file(&base.join(format!("ctream-live-{}.gestures", interactive_session()?)))
}

pub(super) fn check_server(stream: &Stream) -> io::Result<()> {
  let Stream::NamedPipe(pipe) = stream;
  with_user(|sid| {
    let mut owner = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: the named-pipe handle stays open. The owner is contained in the
    // returned descriptor and is compared before its allocation is released.
    unsafe {
      let status = GetSecurityInfo(
        pipe.as_handle().as_raw_handle(),
        SE_KERNEL_OBJECT,
        OWNER_SECURITY_INFORMATION,
        &mut owner,
        null_mut(),
        null_mut(),
        null_mut(),
        &mut descriptor,
      );
      if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
      }
      let same = !owner.is_null() && EqualSid(owner, sid) != 0;
      LocalFree(descriptor);
      if same {
        Ok(())
      } else {
        Err(io::Error::new(
          io::ErrorKind::PermissionDenied,
          "live pipe has a different owner",
        ))
      }
    }
  })
}

pub(in crate::daemon) fn check_client(_: &Stream) -> io::Result<()> {
  // The kernel enforces the protected per-user DACL when clients open the pipe.
  Ok(())
}
