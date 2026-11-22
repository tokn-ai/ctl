fn main() {
  println!("cargo:rerun-if-changed=src/macos/screen_capture_kit.m");
  if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
    return;
  }
  let mut build = cc::Build::new();
  build
    .file("src/macos/screen_capture_kit.m")
    .flag("-fobjc-arc");
  if std::env::var_os("MACOSX_DEPLOYMENT_TARGET").is_none() {
    // Use Rust's baseline instead of cc's default of the build machine's OS.
    let min_version = if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64") {
      "-mmacosx-version-min=10.12"
    } else {
      "-mmacosx-version-min=11.0"
    };
    build.flag(min_version);
  }
  build.compile("ctream_screen_capture_kit");
  for framework in ["Foundation", "CoreGraphics", "CoreMedia", "CoreVideo"] {
    println!("cargo:rustc-link-lib=framework={framework}");
  }
}
