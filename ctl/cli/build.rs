fn main() {
  ctl_core::ctld_bundle_build::write(std::path::Path::new("ctl/cli"));

  println!("cargo:rustc-check-cfg=cfg(ctl_repository_tui_tests)");
  let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
  let terminal_fixtures = manifest.join("../../apps/tui/tests/support/mod.rs");
  // Repository tests reuse the TUI's process fixtures. Published crates do not
  // contain these external files; their broker unit tests remain self-contained.
  if terminal_fixtures.is_file() {
    println!("cargo:rustc-cfg=ctl_repository_tui_tests");
    println!("cargo:rerun-if-changed={}", terminal_fixtures.display());
  }
}
