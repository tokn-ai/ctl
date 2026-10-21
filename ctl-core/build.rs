mod build_support;
mod bundle_channel;

fn main() {
  println!("cargo:rerun-if-env-changed=CTL_BUNDLE_CHANNEL");
  let channel_manifest_dir =
    std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
  let explicit_channel = std::env::var("CTL_BUNDLE_CHANNEL").ok();
  let channel = bundle_channel::select(
    explicit_channel.as_deref(),
    channel_manifest_dir.join(".cargo_vcs_info.json").is_file(),
    &std::env::var("CARGO_PKG_VERSION").unwrap(),
  )
  .expect("invalid component channel configuration");
  println!("cargo:rustc-env=CTL_BUNDLE_CHANNEL={channel}");
  println!(
    "cargo:rustc-env=COMPONENT_TARGET={}",
    std::env::var("TARGET").unwrap()
  );
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
