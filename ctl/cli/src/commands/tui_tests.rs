use super::*;
use ctl_ipc::{ClientMessage, PromptKind, ServerMessage};
use portable_pty::CommandBuilder;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tokio::{net::UnixListener, sync::Notify, task::JoinSet, time::timeout};

#[path = "../../../../apps/tui/tests/support/mod.rs"]
#[allow(dead_code)]
mod support;
use support::{Result, Screen, TestDaemon, TestProxy, Tui};

const CHILD_TEST: &str = "commands::tui_tests::terminal_fixture";
const PROMPT_TEXT: &str = "FORBIDDEN_BROKER_PROMPT";
const WARNING_TEXT: &str = "FORBIDDEN_BROKER_WARNING";

struct Broker {
  prompt: Arc<AtomicUsize>,
  cancelled: Arc<AtomicUsize>,
  changed: Arc<Notify>,
  task: tokio::task::JoinHandle<()>,
}

impl Broker {
  fn start(socket: &std::path::Path) -> Result<Self> {
    let listener = UnixListener::bind(socket)?;
    let prompt = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicUsize::new(0));
    let changed = Arc::new(Notify::new());
    let mode = Arc::clone(&prompt);
    let progress = Arc::clone(&cancelled);
    let notified = Arc::clone(&changed);
    let task = tokio::spawn(async move {
      let mut requests = JoinSet::new();
      loop {
        tokio::select! {
          accepted = listener.accept() => {
            let (stream, _) = accepted.expect("fixture broker accept");
            let mode = Arc::clone(&mode);
            let progress = Arc::clone(&progress);
            let notified = Arc::clone(&notified);
            requests.spawn(async move {
              let prompt = mode.load(Ordering::Acquire);
              answer_request(stream, prompt).await;
              if prompt != 0 {
                progress.fetch_add(1, Ordering::Release);
                notified.notify_one();
              }
            });
          }
          result = requests.join_next(), if !requests.is_empty() => {
            result.expect("request exists").expect("fixture broker request panicked");
          }
        }
      }
    });
    Ok(Self {
      prompt,
      cancelled,
      changed,
      task,
    })
  }

  async fn wait_cancelled(&self, expected: usize) -> Result<()> {
    timeout(Duration::from_secs(5), async {
      loop {
        let notified = self.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        assert!(!self.task.is_finished(), "fixture broker stopped");
        if self.cancelled.load(Ordering::Acquire) >= expected {
          return;
        }
        notified.await;
      }
    })
    .await
    .map_err(|_| "UI reconnect did not cancel the broker prompt".into())
  }
}

impl Drop for Broker {
  fn drop(&mut self) {
    self.task.abort();
  }
}

async fn answer_request(mut stream: tokio::net::UnixStream, prompt: usize) {
  assert!(matches!(
    ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
      .await
      .unwrap(),
    Some(ClientMessage::Handshake { .. })
  ));
  ctl_ipc::write_frame(
    &mut stream,
    &ServerMessage::HandshakeAccepted {
      protocol_version: ctl_ipc::PROTOCOL_VERSION,
    },
  )
  .await
  .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, ClientMessage>(&mut stream).await.unwrap(),
    Some(ClientMessage::EnsureMaster { target } | ClientMessage::EnsureMasterQuiet { target })
      if target.destination == "fixture"
  ));
  if prompt == 0 {
    ctl_ipc::write_frame(
      &mut stream,
      &ServerMessage::MasterReady {
        control_path: PathBuf::from("/tmp/unused-fixture-master"),
      },
    )
    .await
    .unwrap();
  } else {
    ctl_ipc::write_frame(
      &mut stream,
      &ServerMessage::Prompt {
        prompt_id: "fixture-prompt".into(),
        kind: if prompt == 1 {
          PromptKind::Secret
        } else {
          PromptKind::Confirm
        },
        message: PROMPT_TEXT.into(),
        warning: Some(WARNING_TEXT.into()),
      },
    )
    .await
    .unwrap();
    assert!(
      matches!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut stream).await.unwrap(),
        Some(ClientMessage::PromptResponse { prompt_id, response })
          if prompt_id == "fixture-prompt" && response.is_none()
      ),
      "the UI must cancel authentication without reading the host terminal"
    );
  }
}

fn footer(screen: &Screen) -> &str {
  screen.rows.last().map_or("", String::as_str)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_ownership_disables_broker_prompts_through_repeated_reconnects() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session(
      "broker-reconnect",
      "shell",
      ctmux_proto::TerminalSize {
        columns: 120,
        rows: 12,
        ..Default::default()
      },
    )
    .await?;
  let proxy = TestProxy::start(&daemon)?;
  let socket = daemon.directory.join("broker.sock");
  let broker = Broker::start(&socket)?;
  let mut command = CommandBuilder::new(std::env::current_exe()?);
  command.args(["--ignored", "--exact", CHILD_TEST, "--nocapture"]);
  command.env_clear();
  command.env("HOME", daemon.directory.join("home"));
  command.env("PATH", "/usr/bin:/bin");
  command.env("TERM", "xterm-256color");
  command.env("LANG", "C.UTF-8");
  command.env("CTLD_SOCKET_PATH", &socket);
  command.env("CTL_TUI_TEST_SOCKET_PATH", &proxy.socket);
  command.env("CTL_TUI_TEST_SESSION", &session);
  command.cwd(&daemon.directory);
  let mut tui = Tui::spawn(command, 120, 13)?;
  tui
    .wait_screen("initial shell and connected footer", |screen| {
      screen.contains("shell:ready") && footer(screen).starts_with(" connected |")
    })
    .await?;
  for cycle in 0..3 {
    let attachments = proxy.attachments();
    let cancelled = broker.cancelled.load(Ordering::Acquire);
    broker.prompt.store(1 + cycle % 2, Ordering::Release);
    proxy.interrupt();
    broker.wait_cancelled(cancelled + 2).await?;
    tui
      .wait_screen("controlled authentication notice in the footer", |screen| {
        footer(screen).contains("authentication") && screen.contains("shell:ready")
      })
      .await?;
    assert!(!tui.transcript_contains(PROMPT_TEXT.as_bytes()));
    assert!(!tui.transcript_contains(WARNING_TEXT.as_bytes()));
    // Local UI controls stay usable while a connection needs authentication.
    tui.send(b"\x02?")?;
    tui
      .wait_screen("prefix help while disconnected", |screen| {
        screen.contains("Commands") || screen.contains("commands") && screen.contains("detach")
      })
      .await?;
    tui.send(b"\x1b")?;
    broker.prompt.store(0, Ordering::Release);
    proxy.resume();
    proxy.wait_attachments(attachments + 1).await?;
    tui
      .wait_screen("connected screen after cancelling prompts", |screen| {
        footer(screen).starts_with(" connected |") && screen.contains("shell:ready")
      })
      .await?;
    let marker = format!("after-reconnect-{cycle}");
    tui.send(format!("{marker}\r").as_bytes())?;
    tui
      .wait_screen("shell input after broker recovery", |screen| {
        screen.contains(&format!("shell:{marker}"))
      })
      .await?;
  }
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  drop(broker);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test]
#[ignore = "private real-terminal subprocess fixture"]
async fn terminal_fixture() {
  let socket = std::env::var_os("CTL_TUI_TEST_SOCKET_PATH").expect("fixture socket");
  let session = std::env::var("CTL_TUI_TEST_SESSION").expect("fixture session");
  let connector = CtlConnector {
    // Exercise the real SSH prerequisite policy using a private local daemon
    // for service bytes, avoiding an unrelated OpenSSH process in this fixture.
    target: ConnectionTarget::Local {
      socket_path: socket.into(),
    },
    settings: ctl_client::hosts::ConnectionTargetDto::ssh("fixture"),
    recovery: Arc::default(),
    terminal_ui_active: Arc::default(),
  };
  ctmux_cli::run_tui(&connector, Some(session), false)
    .await
    .unwrap();
  assert!(!connector.terminal_ui_active.load(Ordering::Acquire));
}
