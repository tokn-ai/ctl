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

pub(super) fn path() -> io::Result<PathBuf> {
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

pub(super) fn fence_file() -> io::Result<std::fs::File> {
  use std::{
    fs,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
  };
  // LocalAppData inherits the user's profile ACL. Keep this stable across
  // daemon protocol versions so recovery also fences an upgraded executable.
  let base = dirs::data_local_dir().ok_or_else(|| io::Error::other("missing LocalAppData"))?;
  fs::create_dir_all(&base)?;
  let name = format!("ctream-live-{}.gestures", interactive_session()?);
  let file = fs::OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .truncate(false)
    .custom_flags(0x0020_0000)
    .open(base.join(name))?;
  let info = file.metadata()?;
  if !info.is_file() || info.file_attributes() & 0x400 != 0 {
    return Err(io::Error::other("unsafe gesture fence"));
  }
  Ok(file)
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
