fn main() {
  println!("cargo:rustc-check-cfg=cfg(ctld_repository_vpn_tests)");
  // Published crates omit Docker scripts; this regression runs in the repository.
  let scripts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/openconnect");
  if scripts.join("healthcheck.sh").is_file() && scripts.join("vpn-network.sh").is_file() {
    println!("cargo:rustc-cfg=ctld_repository_vpn_tests");
  }
  println!("cargo:rerun-if-changed=../../docker/openconnect/healthcheck.sh");
  println!("cargo:rerun-if-changed=../../docker/openconnect/vpn-network.sh");
  println!("cargo:rerun-if-changed=build.rs");
}
