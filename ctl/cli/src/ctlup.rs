fn main() {
  ctl_core::retain_component_metadata!("ctlup", &ctl_cli::COMPONENT_CONTRACTS);
  ctl_cli::run_updater();
}
