mod bundle_build;
use ctl_core::development as development_build;

fn main() {
  println!("cargo:rustc-check-cfg=cfg(ctl_repository_tui_tests)");
  let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
  let terminal_fixtures = manifest.join("../../apps/tui/tests/support/mod.rs");
  // Repository tests reuse the TUI's process fixtures. Published crates do not
  // contain these external files; their broker unit tests remain self-contained.
  if terminal_fixtures.is_file() {
    println!("cargo:rustc-cfg=ctl_repository_tui_tests");
    println!("cargo:rerun-if-changed={}", terminal_fixtures.display());
  }
  println!("cargo:rerun-if-env-changed=CTL_BUNDLED_CTLD_DIR");
  println!("cargo:rerun-if-env-changed=CTL_BUNDLED_CTLD_MODE");
  println!("cargo:rerun-if-changed=build.rs");
  println!("cargo:rerun-if-changed=bundle_build.rs");
  let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
  let source = std::env::var_os("CTL_BUNDLED_CTLD_DIR").map(std::path::PathBuf::from);
  let target = std::env::var("TARGET").unwrap();
  let development = if source.is_none() {
    development_build::context(
      &std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()),
      std::path::Path::new("ctl/cli"),
      &output,
      &target,
    )
    .expect("could not determine local development helper provenance")
  } else {
    None
  };
  development_build::write(&output, development.as_ref())
    .expect("could not write local development helper provenance");
  let (mode, generated_mode) = match std::env::var("CTL_BUNDLED_CTLD_MODE") {
    Err(std::env::VarError::NotPresent) => (bundle_build::Mode::Signed, "Signed"),
    Ok(value) if value == "signed" => (bundle_build::Mode::Signed, "Signed"),
    Ok(value) if value == "development" => (bundle_build::Mode::Development, "Development"),
    _ => panic!("CTL_BUNDLED_CTLD_MODE must be signed or development"),
  };
  let generated = if let Some(source) = source {
    bundle_build::stage(
      &source,
      &output,
      &std::env::var("CARGO_PKG_VERSION").unwrap(),
      &target,
      mode,
    )
    .expect("invalid bundled ctld payload");
    format!(
      "Some(EmbeddedBundle {{ manifest: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ctld-manifest.json\")), archive: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ctld.app.tar.gz\")), mode: BundleMode::{generated_mode} }})"
    )
  } else {
    "None".to_owned()
  };
  std::fs::write(
    output.join("bundled_ctld.rs"),
    format!("const BUNDLED_CTLD: Option<EmbeddedBundle> = {generated};\n"),
  )
  .unwrap();
}
