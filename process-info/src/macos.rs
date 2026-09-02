use crate::{ProcessIdentity, ProcessInfo, Source, process_name, valid_pid};
use libproc::bsd_info::BSDInfo;
use libproc::proc_pid::{PIDInfo, PidInfoFlavor, pidinfo};
use libproc::processes::{ProcFilter, pids_by_type};
use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

pub(super) struct MacOs;

// Darwin's sys/proc.h process status SZOMB.
const ZOMBIE_STATUS: u32 = 5;

// libproc's pidcwd is unimplemented on macOS. Its safe, extensible PIDInfo
// interface can query Darwin's native vnode-path structure directly instead.
#[repr(transparent)]
struct VnodePathInfo(libc::proc_vnodepathinfo);

impl PIDInfo for VnodePathInfo {
  fn flavor() -> PidInfoFlavor {
    PidInfoFlavor::VNodePathInfo
  }
}

impl Source for MacOs {
  fn process(&self, pid: u32) -> io::Result<ProcessInfo> {
    let info = pidinfo::<BSDInfo>(valid_pid(pid)?, 0).map_err(io::Error::other)?;
    if info.pbi_pid != pid || info.pbi_status == ZOMBIE_STATUS {
      return Err(io::Error::from(io::ErrorKind::NotFound));
    }
    let start_time = info
      .pbi_start_tvsec
      .checked_mul(1_000_000)
      .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec))
      .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
    let name =
      process_name(&c_bytes(&info.pbi_name)).or_else(|| process_name(&c_bytes(&info.pbi_comm)));
    Ok(ProcessInfo {
      identity: ProcessIdentity { pid, start_time },
      parent_pid: info.pbi_ppid,
      process_group: info.pbi_pgid,
      name,
    })
  }

  fn cwd(&self, pid: u32) -> io::Result<PathBuf> {
    let info = pidinfo::<VnodePathInfo>(valid_pid(pid)?, 0).map_err(io::Error::other)?;
    let bytes = c_bytes(info.0.pvi_cdir.vip_path.as_flattened());
    if bytes.is_empty() {
      return Err(io::Error::from(io::ErrorKind::NotFound));
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
  }

  fn group_members(&self, group: u32) -> io::Result<Vec<u32>> {
    valid_pid(group)?;
    pids_by_type(ProcFilter::ByProgramGroup { pgrpid: group })
  }
}

fn c_bytes(chars: &[libc::c_char]) -> Vec<u8> {
  chars
    .iter()
    .take_while(|byte| **byte != 0)
    .map(|byte| byte.to_ne_bytes()[0])
    .collect()
}
