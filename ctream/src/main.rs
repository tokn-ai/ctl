use ctream::{Display, discover};
use std::{env, process::ExitCode};

fn main() -> ExitCode {
  let args: Vec<_> = env::args_os().skip(1).collect();
  if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
    println!("Usage: ctream list\nList listening X11 and Wayland displays on Linux.");
    return ExitCode::SUCCESS;
  }
  if args.len() != 1 || args[0] != "list" {
    eprintln!("Usage: ctream list");
    return ExitCode::from(2);
  }
  let displays = match discover() {
    Ok(displays) => displays,
    Err(error) => {
      eprintln!("ctream: cannot discover Linux display sockets: {error}");
      return ExitCode::FAILURE;
    }
  };
  println!("PROTOCOL\tDISPLAY\tUID\tSESSION CLASS");
  for (display, class) in displays {
    match display {
      Display::X11(number) => println!("X11\t:{number}\t-\t-"),
      Display::Wayland { uid, socket } => println!(
        "Wayland\t{socket}\t{uid}\t{}",
        class.as_deref().unwrap_or("unknown")
      ),
    }
  }
  ExitCode::SUCCESS
}
