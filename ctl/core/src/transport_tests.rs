use super::*;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

// Exercise real OS process pipes, including Windows binary stdio. These
// fixtures model the SSH child's stream boundary, not SSH authentication.
fn fixture(unix: &str, windows_script: &str) -> Command {
  #[cfg(unix)]
  {
    let _ = windows_script;
    let mut command = Command::new("sh");
    command.args(["-c", unix]);
    command
  }
  #[cfg(windows)]
  {
    let _ = unix;
    let mut command = Command::new("powershell.exe");
    command.args([
      "-NoLogo",
      "-NoProfile",
      "-NonInteractive",
      "-ExecutionPolicy",
      "Bypass",
      "-File",
    ]);
    // Keep stdin exclusively for the binary protocol. Windows PowerShell
    // command mode can consume redirected input before the fixture runs.
    command.arg(
      std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(windows_script),
    );
    command
  }
}

#[tokio::test]
async fn transport_consumes_marker_and_preserves_binary_io() {
  timeout(TEST_TIMEOUT, async {
    let command = fixture("printf 'ctl-ssh-v1\n'; cat", "echo-transport.ps1");
    let mut transport = start_ssh_transport(command).await.unwrap();
    let payload = [0, 255, 128, b'\r', b'\n', 27, 1, b'x'];
    transport.write_all(&payload).await.unwrap();
    transport.flush().await.unwrap();
    let mut response = [0; 8];
    transport.read_exact(&mut response).await.unwrap();
    assert_eq!(response, payload);
  })
  .await
  .expect("process-pipe round trip timed out");
}

#[tokio::test]
async fn startup_rejects_stdout_noise_and_retains_stderr() {
  timeout(TEST_TIMEOUT, async {
    let noisy = fixture(
      "printf 'unexpected startup output\n'",
      "noisy-transport.ps1",
    );
    assert!(matches!(
      start_ssh_transport(noisy).await,
      Err(CoreError::InvalidSshPreface)
    ));
    let failed = fixture(
      "printf 'Host key verification failed.\n' >&2; exit 255",
      "failed-transport.ps1",
    );
    let Err(CoreError::SshStartup(message)) = start_ssh_transport(failed).await else {
      panic!("expected SSH startup diagnostics");
    };
    assert!(message.contains("Host key verification failed."));
  })
  .await
  .expect("startup failure handling timed out");
}

// Native Windows CI must also exercise the installed OpenSSH executable,
// without credentials, a real remote account, or host-key policy changes.
#[cfg(windows)]
#[tokio::test]
async fn windows_openssh_reports_connection_failure() {
  timeout(TEST_TIMEOUT, async {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let options = SshConnectionOptions {
      hostname: Some("127.0.0.1".into()),
      port: Some(listener.local_addr().unwrap().port()),
      ..SshConnectionOptions::default()
    };
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      stream.write_all(b"SSH-2.0-ctl-test\r\n").await.unwrap();
      let mut buffer = [0; 1024];
      let _ = stream.read(&mut buffer).await;
      // Close before key exchange; no authentication can occur.
    });
    let result = open_ssh_tunnel_interactive("fixture", &options, &SshInteraction::Batch).await;
    assert!(matches!(result, Err(CoreError::SshStartup(_))));
    server.await.unwrap();
  })
  .await
  .expect("Windows OpenSSH startup timed out");
}
