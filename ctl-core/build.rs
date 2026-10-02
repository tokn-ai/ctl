mod build_support;

fn main() {
  let manifest_dir = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
  let identity = build_support::read_identity(&manifest_dir);
  println!(
    "cargo:rustc-env=COMPONENT_SOURCE_FINGERPRINT={}",
    identity.fingerprint
  );
  println!(
    "cargo:rustc-env=COMPONENT_SOURCE_REVISION={}",
    identity.revision.unwrap_or_default()
  );
  println!("cargo:rustc-env=COMPONENT_SOURCE_DIRTY={}", identity.dirty);
}
