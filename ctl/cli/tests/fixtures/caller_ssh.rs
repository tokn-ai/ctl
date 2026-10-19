// Standalone OpenSSH process fixture, compiled by the caller integration tests.
use std::{
  io::{Read, Write},
  process::{Command, Output, Stdio},
};

const PASSWORD: &str = "  caller-credential-☃-canary  ";
const FINGERPRINT: &str = "SHA256:abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";
const TRUST_PROMPT: &str = "The authenticity of host 'fixture (127.0.0.1)' can't be established.\n\
ED25519 key fingerprint is SHA256:abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ.\n\
This key is not known by any other names.\n\
Are you sure you want to continue connecting (yes/no/[fingerprint])? ";
// Newer OpenSSH uses a colon and no trailing period on the fingerprint line.
const MODERN_TRUST_PROMPT: &str = "The authenticity of host 'fixture (127.0.0.1)' can't be established.\n\
ED25519 key fingerprint is: SHA256:abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ\n\
This key is not known by any other names.\n\
Are you sure you want to continue connecting (yes/no/[fingerprint])? ";

fn event(value: &str) {
  if let Some(path) = std::env::var_os("CTL_TEST_TRUST_EVENTS") {
    writeln!(
      std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .unwrap(),
      "{value}"
    )
    .unwrap();
  }
}

fn askpass(prompt: &str, confirmation: bool) -> Output {
  let mut command = Command::new(std::env::var_os("SSH_ASKPASS").expect("askpass program"));
  command.arg(prompt).env_remove("SSH_ASKPASS_PROMPT");
  if confirmation {
    command.env("SSH_ASKPASS_PROMPT", "confirm");
  }
  command.stdin(Stdio::null()).output().unwrap()
}

fn reject_trust() -> ! {
  event("trust-rejected");
  eprintln!("Host key verification failed.");
  std::process::exit(255)
}

fn host_trust(args: &[String], scenario: &str) {
  let policy = if matches!(scenario, "unknown" | "unknown-modern" | "changed") {
    "stricthostkeychecking=ask"
  } else {
    "stricthostkeychecking=yes"
  };
  assert!(
    args
      .iter()
      .any(|argument| argument.eq_ignore_ascii_case(policy)),
    "the configured host-key policy was not preserved"
  );
  event("trust-check");
  match scenario {
    "" | "trusted" | "encrypted" => event("trust-known"),
    "changed" => {
      eprintln!("@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@");
      reject_trust();
    }
    "strict" => reject_trust(),
    "unknown" | "unknown-modern" => {
      // OpenSSH does not ask in batch mode. For forced askpass it invokes the
      // full first-use question without SSH_ASKPASS_PROMPT=confirm.
      if args
        .iter()
        .any(|arg| arg.eq_ignore_ascii_case("BatchMode=yes"))
      {
        reject_trust();
      }
      assert_eq!(std::env::var("SSH_ASKPASS_REQUIRE").unwrap(), "force");
      let prompt = if scenario == "unknown-modern" {
        MODERN_TRUST_PROMPT
      } else {
        TRUST_PROMPT
      };
      let result = askpass(prompt, false);
      std::io::stderr().write_all(&result.stderr).unwrap();
      assert!(
        !result
          .stdout
          .windows(PASSWORD.len())
          .any(|value| value == PASSWORD.as_bytes())
      );
      let answer = String::from_utf8_lossy(&result.stdout);
      if !result.status.success() || !matches!(answer.trim(), "yes" | FINGERPRINT) {
        reject_trust();
      }
      std::fs::write(
        std::env::var_os("CTL_TEST_TRUST_FILE").unwrap(),
        "fixture-known-key\n",
      )
      .unwrap();
      event("trust-approved");
    }
    _ => panic!("unknown fixture trust scenario"),
  }
}

fn main() {
  let args: Vec<_> = std::env::args().skip(1).collect();
  let scenario = std::env::var("CTL_TEST_TRUST_CASE").unwrap_or_default();
  #[cfg(unix)]
  assert!(
    std::fs::metadata("/dev/fd/3").is_err(),
    "credential descriptor leaked into SSH"
  );
  if args.contains(&"-G".to_owned()) {
    let policy = if matches!(scenario.as_str(), "unknown" | "unknown-modern" | "changed") {
      "ask"
    } else {
      "yes"
    };
    println!(
      "hostname 127.0.0.1\nuser caller\nport 2222\nstricthostkeychecking {policy}\nidentityfile /unrelated-authorized-key\nproxycommand none"
    );
    match std::env::var("CTL_TEST_CONFIG_PATHS")
      .unwrap_or_default()
      .as_str()
    {
      "default-space" => println!(
        "userknownhostsfile C:/Users/Two Names/.ssh/known_hosts C:/Users/Two Names/.ssh/known_hosts2"
      ),
      "custom-space" if !args.contains(&"-F".to_owned()) => {
        println!("userknownhostsfile /custom/trust store/known_hosts")
      }
      _ => println!("userknownhostsfile /default/known_hosts"),
    }
    return;
  }
  assert!(!args.iter().any(|arg| arg.contains(PASSWORD)));
  assert!(!std::env::vars_os().any(|(_, value)| value.to_string_lossy().contains(PASSWORD)));
  let has = |value: &str| args.iter().any(|arg| arg == value);
  for value in [
    "none",
    "ControlPath=none",
    "IdentityAgent=none",
    "IdentitiesOnly=yes",
    "CertificateFile=none",
    "ForwardAgent=no",
  ] {
    assert!(has(value), "missing authentication isolation option");
  }
  assert!(!has("/unrelated-authorized-key"));
  assert!(
    !args.iter().any(|arg| arg.contains("Two Names")),
    "default paths were not inherited losslessly"
  );
  host_trust(&args, &scenario);
  if scenario == "encrypted" {
    let response = askpass("Enter passphrase for key '/fixture/encrypted-key': ", false);
    assert!(!response.status.success());
    assert!(response.stdout.is_empty());
    event("passphrase-rejected");
    std::process::exit(255);
  }
  if has("PreferredAuthentications=password") {
    event("authenticate-password");
    assert!(has("PubkeyAuthentication=no"));
    let result = askpass("caller@host's password: ", false);
    assert!(result.status.success(), "askpass failed");
    if result.stdout != format!("{PASSWORD}\n").as_bytes() {
      eprintln!("Permission denied (password).");
      std::process::exit(255);
    }
    // A confirmation request that is not a complete first-use host question
    // must never receive the supplied password or be approved.
    let confirmation = askpass(
      "Are you sure you want to continue connecting (yes/no)?",
      true,
    );
    assert!(!confirmation.status.success());
    assert!(confirmation.stdout.is_empty());
  } else {
    event("authenticate-key");
    assert!(has("PreferredAuthentications=publickey"));
    assert!(has("PasswordAuthentication=no"));
    assert!(has("-i"));
    assert_eq!(std::env::var("SSH_ASKPASS_REQUIRE").unwrap(), "force");
    assert!(std::env::var_os("CTL_CALLER_ENDPOINT").is_none());
    assert!(std::env::var_os("CTL_CALLER_TOKEN").is_none());
    let response = askpass("caller@host's password: ", false);
    assert!(!response.status.success(), "key mode supplied a password");
    assert!(response.stdout.is_empty());
  }
  event("command");
  let mut input = Vec::new();
  std::io::stdin().read_to_end(&mut input).unwrap();
  std::io::stdout().write_all(&input).unwrap();
  eprintln!("separate-command-stderr");
  std::process::exit(17);
}
