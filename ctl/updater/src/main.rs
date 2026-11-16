fn main() {
  ctl_core::retain_component_metadata!("ctlup", &ctlup::COMPONENT_CONTRACTS);
  ctlup::run();
}
