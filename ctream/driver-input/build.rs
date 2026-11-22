fn main() {
  println!("cargo:rerun-if-changed=src/macos/window_input.m");
  if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
    return;
  }
  let mut build = cc::Build::new();
  build.file("src/macos/window_input.m").flag("-fobjc-arc");
  if std::env::var_os("MACOSX_DEPLOYMENT_TARGET").is_none() {
    let min_version = if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64") {
      "-mmacosx-version-min=10.12"
    } else {
      "-mmacosx-version-min=11.0"
    };
    build.flag(min_version);
  }
  build.compile("ctream_macos_window_input");
  for framework in [
    "AppKit",
    "ApplicationServices",
    "CoreGraphics",
    "Foundation",
  ] {
    println!("cargo:rustc-link-lib=framework={framework}");
  }
}
