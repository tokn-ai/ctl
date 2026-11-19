#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
  let args: Vec<_> = std::env::args().skip(1).collect();
  match args.as_slice() {
    [operation] if operation == "list" => linux::run(None),
    [operation, id] if operation == "capture" => linux::run(Some(id)),
    _ => Err("usage: ctream-wayland-window list | capture ID".into()),
  }
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
  eprintln!("ctream-wayland-window requires Linux");
  std::process::ExitCode::FAILURE
}
