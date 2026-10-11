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
  watches: Arc<AtomicUsize>,
  closed_watches: Arc<AtomicUsize>,
  changed: Arc<Notify>,
  task: tokio::task::JoinHandle<()>,
}

impl Broker {
  fn start(socket: &std::path::Path) -> Result<Self> {
    let listener = UnixListener::bind(socket)?;
    let prompt = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicUsize::new(0));
    let watches = Arc::new(AtomicUsize::new(0));
    let closed_watches = Arc::new(AtomicUsize::new(0));
    let changed = Arc::new(Notify::new());
    let mode = Arc::clone(&prompt);
    let progress = Arc::clone(&cancelled);
    let notified = Arc::clone(&changed);
    let registrations = Arc::clone(&watches);
    let closed = Arc::clone(&closed_watches);
    let task = tokio::spawn(async move {
      let mut requests = JoinSet::new();
      loop {
        tokio::select! {
          accepted = listener.accept() => {
            let (stream, _) = accepted.expect("fixture broker accept");
            let mode = Arc::clone(&mode);
            let progress = Arc::clone(&progress);
            let notified = Arc::clone(&notified);
            let registrations = Arc::clone(&registrations);
            let closed = Arc::clone(&closed);
            requests.spawn(async move {
              answer_request(stream, mode, progress, registrations, closed, notified).await;
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
      watches,
      closed_watches,
      changed,
      task,
    })
  }

  async fn wait_cancelled(&self, expected: usize) -> Result<()> {
    self.wait_counter(&self.cancelled, expected).await
  }

  async fn wait_counter(&self, counter: &AtomicUsize, expected: usize) -> Result<()> {
    timeout(Duration::from_secs(5), async {
      loop {
        let notified = self.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        assert!(!self.task.is_finished(), "fixture broker stopped");
        if counter.load(Ordering::Acquire) >= expected {
          return;
        }
        notified.await;
      }
    })
    .await
    .map_err(|_| "UI reconnect did not reach the expected broker state".into())
  }
}

impl Drop for Broker {
  fn drop(&mut self) {
    self.task.abort();
  }
}

async fn answer_request(
  mut stream: tokio::net::UnixStream,
  prompt: Arc<AtomicUsize>,
  cancelled: Arc<AtomicUsize>,
  watches: Arc<AtomicUsize>,
  closed_watches: Arc<AtomicUsize>,
  changed: Arc<Notify>,
) {
  assert!(matches!(
    ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
      .await
      .unwrap(),
    Some(ClientMessage::Handshake { .. })
  ));
  ctl_ipc::write_frame(
    &mut stream,
    &ServerMessage::HandshakeAccepted {
      protocol_version: ctl_ipc::CONTRACT_V1_1_16,
    },
  )
  .await
  .unwrap();
  while let Some(request) = ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
    .await
    .unwrap()
  {
    if let ClientMessage::WatchRecovery { target } = request {
      assert_eq!(target.destination, "fixture");
      watches.fetch_add(1, Ordering::Release);
      changed.notify_one();
      ctl_ipc::write_frame(&mut stream, &ServerMessage::RecoveryRegistered)
        .await
        .unwrap();
      assert!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
          .await
          .unwrap()
          .is_none(),
        "a recovery watch must stay passive until the terminal releases it"
      );
      closed_watches.fetch_add(1, Ordering::Release);
      changed.notify_one();
      return;
    }
    if answer_master(&mut stream, request, prompt.load(Ordering::Acquire)).await {
      cancelled.fetch_add(1, Ordering::Release);
      changed.notify_one();
    }
  }
}

async fn answer_master(
  stream: &mut tokio::net::UnixStream,
  request: ClientMessage,
  prompt: usize,
) -> bool {
  let passive = matches!(&request, ClientMessage::MasterStatus { .. });
  assert!(matches!(
    request,
    ClientMessage::EnsureMaster { target }
      | ClientMessage::EnsureMasterQuiet { target }
      | ClientMessage::MasterStatus { target } if target.destination == "fixture"
  ));
  if passive && prompt != 0 {
    ctl_ipc::write_frame(&mut *stream, &ServerMessage::AuthenticationRequired)
      .await
      .unwrap();
    return false;
  }
  if prompt == 0 {
    ctl_ipc::write_frame(
      &mut *stream,
      &ServerMessage::MasterReady {
        control_path: PathBuf::from("/tmp/unused-fixture-master"),
      },
    )
    .await
    .unwrap();
  } else {
    ctl_ipc::write_frame(
      &mut *stream,
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
        ctl_ipc::read_frame::<_, ClientMessage>(&mut *stream).await.unwrap(),
        Some(ClientMessage::PromptResponse { prompt_id, response })
          if prompt_id == "fixture-prompt" && response.is_none()
      ),
      "the UI must cancel authentication without reading the host terminal"
    );
  }
  !passive && prompt != 0
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
  broker.wait_counter(&broker.watches, 1).await?;
  assert_eq!(broker.cancelled.load(Ordering::Acquire), 0);
  for cycle in 0..3 {
    let attachments = proxy.attachments();
    let cancelled = broker.cancelled.load(Ordering::Acquire);
    broker.prompt.store(1 + cycle % 2, Ordering::Release);
    proxy.interrupt();
    broker.wait_cancelled(cancelled + 1).await?;
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
    // Authentication is nonretryable: periodic refresh and pane maintenance
    // must stop opening broker sockets until an explicit session retry.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let cancelled = broker.cancelled.load(Ordering::Acquire);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(broker.cancelled.load(Ordering::Acquire), cancelled);
    broker.prompt.store(0, Ordering::Release);
    proxy.resume();
    tui.send(b"\x02s")?;
    tui
      .wait_screen("session picker for explicit reconnect", |screen| {
        screen.contains("broker-reconnect")
      })
      .await?;
    tui.send(b"\r")?;
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
  broker.wait_counter(&broker.closed_watches, 1).await?;
  assert_eq!(broker.watches.load(Ordering::Acquire), 1);
  assert_eq!(broker.closed_watches.load(Ordering::Acquire), 1);
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
    #[cfg(unix)]
    recovery_watch: Arc::default(),
    #[cfg(unix)]
    broker: Arc::default(),
  };
  ctmux_cli::run_tui(&connector, Some(session), false)
    .await
    .unwrap();
  assert!(!connector.terminal_ui_active.load(Ordering::Acquire));
}
