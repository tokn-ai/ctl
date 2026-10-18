// Standalone OpenSSH process fixture, compiled by caller_credentials.rs on each OS.
use std::{io::{Read, Write}, process::{Command, Stdio}};
const PASSWORD: &str = "  caller-credential-☃-canary  ";
fn main() {
  let args: Vec<_> = std::env::args().skip(1).collect();
  #[cfg(unix)]
  assert!(std::fs::metadata("/dev/fd/3").is_err(), "credential descriptor leaked into SSH");
  if args.contains(&"-G".to_owned()) {
    println!("hostname 127.0.0.1\nuser caller\nport 2222\nstricthostkeychecking yes\nidentityfile /unrelated-authorized-key\nproxycommand none");
    return;
  }
  assert!(!args.iter().any(|arg| arg.contains(PASSWORD)));
  assert!(!std::env::vars_os().any(|(_, value)| value.to_string_lossy().contains(PASSWORD)));
  let has = |value: &str| args.iter().any(|arg| arg == value);
  for value in ["none", "ControlPath=none", "IdentityAgent=none", "IdentitiesOnly=yes",
    "CertificateFile=none", "ForwardAgent=no", "stricthostkeychecking=yes"] {
    assert!(has(value), "missing authentication isolation option");
  }
  assert!(!has("/unrelated-authorized-key"));
  if has("PreferredAuthentications=password") {
    assert!(has("PubkeyAuthentication=no"));
    let program = std::env::var_os("SSH_ASKPASS").expect("askpass program");
    let result = Command::new(&program).arg("caller@host's password: ")
      .stdin(Stdio::null()).output().unwrap();
    assert!(result.status.success(), "askpass failed");
    if result.stdout != format!("{PASSWORD}\n").as_bytes() {
      eprintln!("Permission denied (password).");
      std::process::exit(255);
    }
    // A host-key confirmation must never receive the supplied password.
    let confirmation = Command::new(program).arg("Are you sure you want to continue connecting (yes/no)?")
      .env("SSH_ASKPASS_PROMPT", "confirm").stdin(Stdio::null()).output().unwrap();
    assert!(!confirmation.status.success());
    assert!(confirmation.stdout.is_empty());
  } else {
    assert!(has("PreferredAuthentications=publickey"));
    assert!(has("PasswordAuthentication=no"));
    assert!(has("-i"));
    assert!(std::env::var_os("SSH_ASKPASS").is_none());
  }
  let mut input = Vec::new();
  std::io::stdin().read_to_end(&mut input).unwrap();
  std::io::stdout().write_all(&input).unwrap();
  eprintln!("separate-command-stderr");
  std::process::exit(17);
}
