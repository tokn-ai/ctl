use ctl_core::component::{ComponentInfo, ProtocolInfo};
use std::io::{self, Write as _};

/// Contracts consumed by this executable, from the same definitions used in negotiation.
pub fn print() -> io::Result<()> {
  let mut protocols = ctl_proto::agent_protocols();
  protocols.extend(ctl_ipc::lifecycle::DaemonBinaryInfo::current().protocols);
  protocols.extend([
    ctmux_proto::protocol_info(),
    ctmux_ipc::local_control_protocol_info(),
    ctl_task_proto::protocol_info(),
    ctl_task_proto::control::protocol_info(),
    ProtocolInfo::new(
      "ctl_remote_vpn",
      ctl_ipc::remote_vpn::PROTOCOL_BUILD,
      ctl_ipc::remote_vpn::PROTOCOL_VERSION,
      ctl_ipc::remote_vpn::SUPPORTED_PROTOCOL_VERSIONS,
    ),
  ]);
  let component = ComponentInfo {
    build: ctl_core::component::build_info(),
    protocols,
  };
  let mut output = io::stdout().lock();
  serde_json::to_writer(&mut output, &component)?;
  writeln!(output)
}
