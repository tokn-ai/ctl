use ctl_core::component::{ComponentInfo, ProtocolContract};
use std::io::{self, Write as _};

/// Contracts consumed by this executable, from the same definitions used in negotiation.
pub const CONTRACTS: [ProtocolContract; 13] = ProtocolContract::join(&[
  &ctl_proto::AGENT_CONTRACTS,
  &ctl_ipc::lifecycle::DAEMON_CORE_CONTRACTS,
  &ctmux_ipc::COMPONENT_CONTRACTS,
  &[
    ctl_task_proto::CONTRACT,
    ctl_task_proto::control::CONTRACT,
    ctl_ipc::remote_vpn::CONTRACT,
  ],
]);

pub fn print() -> io::Result<()> {
  let component = ComponentInfo::from_contracts(&CONTRACTS);
  let mut output = io::stdout().lock();
  serde_json::to_writer(&mut output, &component)?;
  writeln!(output)
}
