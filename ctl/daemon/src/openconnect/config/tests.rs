use super::*;

#[test]
fn status_metadata_uses_literal_last_values_and_excludes_authentication_secrets() {
  let metadata = parse_metadata(concat!(
    "# comment\nVPN_URL=old.example.test\nVPN_USERNAME=old-user\n",
    "VPN_URL=https://url-user:url-password@vpn.example.test:444/token-path?token=query-secret#fragment-secret\n",
    "VPN_USERNAME=literal $user='name'\\suffix#=tail\n",
    "VPN_PASSWORD=password-secret\nVPN_AUTH_METHOD=method-secret\nTARGET_IP=target-secret"
  )).unwrap();
  assert_eq!(
    metadata.vpn_url.as_deref(),
    Some("https://vpn.example.test:444")
  );
  assert_eq!(
    metadata.username.as_deref(),
    Some("literal $user='name'\\suffix#=tail")
  );
}

#[test]
fn malformed_configs_return_only_static_diagnostics() {
  for content in [
    "VPN_URL=private-value\nVPN_USERNAME=test\nVPN_PASSWORD=password\nUNKNOWN=private-value",
    "VPN_URL=private-value\nVPN_USERNAME=test\nVPN_PASSWORD=password\nprivate-value",
    "VPN_URL=private-value\r\nVPN_USERNAME=test\nVPN_PASSWORD=password",
    "VPN_URL=private-value\nVPN_USERNAME=test\nVPN_PASSWORD=password\0",
    "VPN_URL=private-value\nVPN_USERNAME=test\nVPN_PASSWORD=password\nVPN_PASSWORD=",
    "VPN_URL=http://private-value\nVPN_USERNAME=test\nVPN_PASSWORD=password",
    "VPN_URL=private-value\nVPN_USERNAME=test\u{1b}private-value\nVPN_PASSWORD=password",
  ] {
    let error = parse_metadata(content)
      .err()
      .expect("invalid config must fail");
    assert!(!error.to_string().contains("private-value"));
  }
}

#[test]
fn gateway_status_is_an_https_origin_only() {
  for (input, expected) in [
    ("vpn.example.test", "https://vpn.example.test"),
    ("vpn.example.test/path", "https://vpn.example.test"),
    (
      "https://vpn.example.test/?token=secret",
      "https://vpn.example.test",
    ),
    (
      "https://vpn.example.test/?next=https://redirect.example.test/secret",
      "https://vpn.example.test",
    ),
    (
      "https://user:pass@vpn.example.test#secret",
      "https://vpn.example.test",
    ),
    ("https://[2001:db8::1]:443/path", "https://[2001:db8::1]"),
  ] {
    assert_eq!(display_gateway(input).unwrap(), expected);
  }
  for input in [
    "",
    "https://",
    "https://?secret",
    "http://vpn.example.test",
    "https://user\\host/path",
    "vpn.example.test\u{1b}",
  ] {
    assert!(display_gateway(input).is_err());
  }
}

#[cfg(unix)]
mod files {
  use std::fs;
  use std::os::unix::fs::PermissionsExt as _;

  use super::*;

  struct Fixture(PathBuf);

  impl Fixture {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("ctld-vpn-file-{}", uuid::Uuid::new_v4()));
      fs::create_dir(&path).unwrap();
      Self(path)
    }

    fn config(&self, name: &str, content: &[u8]) -> PathBuf {
      let path = self.0.join(name);
      fs::write(&path, content).unwrap();
      fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
      path
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  const CONTENT: &[u8] =
    b"VPN_URL=vpn.example.test\nVPN_USERNAME=test-user\nVPN_PASSWORD=test-password\n";

  #[tokio::test]
  async fn snapshot_preserves_config_after_file_changes_and_supports_special_paths() {
    let fixture = Fixture::new();
    let path = fixture.config("vpn, literal.env", CONTENT);
    let alias = fixture.0.join("alias.env");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    let (canonical, snapshot) = read_file(alias).await.unwrap();
    fs::write(&path, "VPN_URL=changed.example.test").unwrap();
    assert_eq!(canonical, path.canonicalize().unwrap());
    assert_eq!(snapshot.content.as_bytes(), CONTENT);
    assert_eq!(snapshot.metadata.username.as_deref(), Some("test-user"));
    assert_eq!(
      snapshot.metadata.vpn_url.as_deref(),
      Some("https://vpn.example.test")
    );
  }

  #[tokio::test]
  async fn private_regular_utf8_bounded_files_are_required() {
    let fixture = Fixture::new();
    let public = fixture.config("public.env", CONTENT);
    fs::set_permissions(&public, fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(
      read_file(public).await.err().unwrap().kind(),
      io::ErrorKind::PermissionDenied
    );
    let invalid = fixture.config("invalid.env", &[0xff]);
    assert!(read_file(invalid).await.is_err());
    let large = fixture.config(
      "large.env",
      &vec![b'#'; usize::try_from(MAX_ENV_BYTES).unwrap() + 1],
    );
    assert!(read_file(large).await.is_err());
    assert!(read_file(fixture.0.clone()).await.is_err());
    let fifo = fixture.0.join("fifo.env");
    assert!(
      std::process::Command::new("mkfifo")
        .args(["-m", "600"])
        .arg(&fifo)
        .status()
        .unwrap()
        .success()
    );
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), read_file(fifo))
      .await
      .unwrap();
    assert!(result.is_err());
  }
}
