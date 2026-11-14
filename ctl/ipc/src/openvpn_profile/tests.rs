use super::*;

const PROFILE: &str = "client\r\ndev tun\r\nproto udp\r\nremote vpn.example.test 1194\r\n\
                       <ca>\r\ncertificate-fixture\r\n</ca>\r\n";

#[test]
fn renders_certificates_and_both_supported_password_inputs() {
  let with_prompt = format!("{PROFILE}auth-user-pass\n");
  assert!(validate(&with_prompt, None, None).is_err());
  let output = render(&with_prompt, Some("fixture-user"), Some("fixture-password")).unwrap();
  assert!(output.contains("<auth-user-pass>\nfixture-user\nfixture-password\n</auth-user-pass>\n"));
  assert!(output.contains("remote-cert-tls server\n"));
  assert!(!output.contains(['\r']));
  assert!(!output.contains("dev tun"));

  let inline =
    format!("{PROFILE}<auth-user-pass>\nfixture-user\nfixture-secret\n</auth-user-pass>\n");
  assert!(validate(&inline, None, None).is_ok());
  assert!(validate(&inline, Some("replacement"), Some("replacement")).is_err());
  let public = public_settings(&inline).unwrap().join("\n");
  for private in ["certificate-fixture", "fixture-user", "fixture-secret"] {
    assert!(!public.contains(private));
  }
  assert_eq!(
    public,
    public_settings(&inline.replace("fixture-secret", "new-secret"))
      .unwrap()
      .join("\n")
  );
  assert_ne!(
    public,
    public_settings(&inline.replace("certificate-fixture", "new-certificate"))
      .unwrap()
      .join("\n")
  );
  assert!(validate(PROFILE, Some("fixture"), Some("</auth-user-pass> junk")).is_err());
}

#[test]
fn rejects_file_references_hooks_and_ambiguous_inline_boundaries() {
  for private in [
    "config credentials-fixture",
    "up '/tmp/fixture'",
    "--script-security 3",
    "management 127.0.0.1 7505",
    "auth-user-pass private.txt",
    "ca ca.crt",
    "plugin fixture.so",
    "log profile.txt",
    "dev tap",
    "<connection>\nremote vpn.example.test\n</connection>",
    "<key>\ncontents\n</key> nested\nup fixture\n</key>",
    "<key>\ncontents\n\t</key>extra\nup fixture\n</key>",
    "<key>\ncontents\rstate\n</key>",
  ] {
    let error = validate(&format!("{PROFILE}{private}\n"), None, None).unwrap_err();
    assert!(!error.contains(private));
  }
  let long = format!("{PROFILE}<key>\n{}\n</key>\n", "k".repeat(255));
  assert!(validate(&long, None, None).is_err());
  assert!(validate(&"x".repeat(MAX_CONFIG_BYTES + 1), None, None).is_err());
}

#[test]
fn requires_a_remote_and_server_trust_and_preserves_simple_quoted_options() {
  assert!(validate("client\nremote vpn.example.test\n", None, None).is_err());
  let hash = vec!["AB"; 32].join(":");
  let config = format!(
    "client\nremote vpn.example.test\npeer-fingerprint {hash}\nverify-x509-name 'VPN Server' name\n"
  );
  let output = render(&config, None, None).unwrap();
  assert!(output.contains("verify-x509-name \"VPN Server\" name\n"));
  assert!(output.contains(&hash));
  assert!(!output.contains("remote-cert-tls"));
  assert!(validate("client\n<ca>\nfixture\n</ca>\n", None, None).is_err());
}
