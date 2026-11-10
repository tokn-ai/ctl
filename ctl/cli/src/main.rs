fn main() {
  ctl_core::retain_component_metadata!("ctl", &ctl_cli::COMPONENT_CONTRACTS);
  ctl_cli::run();
}
