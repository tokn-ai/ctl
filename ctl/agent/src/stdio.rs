//! Agent-specific coverage of shared owned SSH descriptors.
#[cfg(test)]
mod tests {
  use std::process::Stdio;
  use std::time::Duration;
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

  #[tokio::test]
  async fn connect_child() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let Some(socket) = std::env::var_os("CTL_AGENT_CONNECT_TEST_SOCKET") else {
      return;
    };
    crate::connect_stdio(&crate::ConnectConfig::new(socket.into()))
      .await
      .unwrap();
  }

  #[tokio::test]
  async fn daemon_eof_finishes_connect_without_waiting_for_stdin() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let socket = std::env::temp_dir().join(format!("ccs-{}.s", uuid::Uuid::new_v4().simple()));
    let daemon = tokio::net::UnixListener::bind(&socket).unwrap();
    let relay = tokio::spawn(async move {
      let (mut stream, _) = daemon.accept().await.unwrap();
      stream.write_all(b"daemon-reply").await.unwrap();
    });
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
      .args(["--exact", "stdio::tests::connect_child", "--nocapture"])
      .env("CTL_AGENT_CONNECT_TEST_SOCKET", &socket)
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::inherit())
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    let _open_input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), output.read_to_end(&mut bytes))
      .await
      .expect("daemon EOF must finish output with client stdin still open")
      .unwrap();
    assert!(bytes.ends_with(b"daemon-reply"));
    assert!(
      tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .expect("runtime shutdown must not wait for a blocking stdin reader")
        .unwrap()
        .success()
    );
    relay.await.unwrap();
    std::fs::remove_file(socket).unwrap();
  }
}
