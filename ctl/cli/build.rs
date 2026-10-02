mod bundle_build;

fn main() {
  println!("cargo:rerun-if-env-changed=CTL_BUNDLED_CTLD_DIR");
  println!("cargo:rerun-if-changed=build.rs");
  println!("cargo:rerun-if-changed=bundle_build.rs");
  let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
  let source = std::env::var_os("CTL_BUNDLED_CTLD_DIR").map(std::path::PathBuf::from);
  let generated = if let Some(source) = source {
    bundle_build::stage(
      &source,
      &output,
      &std::env::var("CARGO_PKG_VERSION").unwrap(),
      &std::env::var("TARGET").unwrap(),
    )
    .expect("invalid bundled ctld payload");
    "Some((include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ctld-manifest.json\")), include_bytes!(concat!(env!(\"OUT_DIR\"), \"/ctld.app.tar.gz\"))))"
  } else {
    "None"
  };
  std::fs::write(
    output.join("bundled_ctld.rs"),
    format!("const BUNDLED_CTLD: Option<(&[u8], &[u8])> = {generated};\n"),
  )
  .unwrap();
}
