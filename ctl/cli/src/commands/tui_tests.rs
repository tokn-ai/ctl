use super::*;
use clap::Parser as _;
use ctl_ipc::{ClientMessage, PromptKind, ServerMessage};
use portable_pty::CommandBuilder;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Duration;
use tokio::{net::UnixListener, sync::Notify, task::JoinSet, time::timeout};

#[path = "../../../../apps/tui/tests/support/mod.rs"]
#[allow(dead_code)]
mod support;
use support::{Result, Screen, TestDaemon, TestProxy, Tui};

const CHILD_TEST: &str = "commands::tui_tests::terminal_fixture";
const RESUME_CHILD_TEST: &str = "commands::tui_tests::resume_terminal_fixture";
const PROMPT_TEXT: &str = "FORBIDDEN_BROKER_PROMPT";
const WARNING_TEXT: &str = "FORBIDDEN_BROKER_WARNING";
const SECRET_RESPONSE: &str = "fixture-private-authentication-marker";

#[derive(Default)]
struct Observations {
  unknown: AtomicBool,
  checks: AtomicUsize,
  failed: AtomicUsize,
  ensures: AtomicUsize,
  interactive: AtomicUsize,
  authenticated: AtomicUsize,
  rejected: AtomicUsize,
}

struct Broker {
  prompt: Arc<AtomicUsize>,
  cancelled: Arc<AtomicUsize>,
  watches: Arc<AtomicUsize>,
  closed_watches: Arc<AtomicUsize>,
  observations: Arc<Observations>,
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
    let observations = Arc::new(Observations::default());
    let changed = Arc::new(Notify::new());
    let mode = Arc::clone(&prompt);
    let progress = Arc::clone(&cancelled);
    let notified = Arc::clone(&changed);
    let registrations = Arc::clone(&watches);
    let closed = Arc::clone(&closed_watches);
    let observed = Arc::clone(&observations);
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
            let observed = Arc::clone(&observed);
            requests.spawn(async move {
              answer_request(stream, mode, progress, registrations, closed, observed, notified).await;
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
      observations,
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
  observations: Arc<Observations>,
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
    if answer_master(&mut stream, request, &prompt, &observations, &changed).await {
      cancelled.fetch_add(1, Ordering::Release);
    }
    changed.notify_one();
  }
}

async fn answer_master(
  stream: &mut tokio::net::UnixStream,
  request: ClientMessage,
  prompt_mode: &AtomicUsize,
  observations: &Observations,
  changed: &Notify,
) -> bool {
  let passive = matches!(&request, ClientMessage::MasterStatus { .. });
  let interactive = matches!(&request, ClientMessage::EnsureMaster { .. });
  let prompt = prompt_mode.load(Ordering::Acquire);
  assert!(matches!(
    request,
    ClientMessage::EnsureMaster { target }
      | ClientMessage::EnsureMasterQuiet { target }
      | ClientMessage::MasterStatus { target } if target.destination == "fixture"
  ));
  if passive {
    observations.checks.fetch_add(1, Ordering::Release);
    if observations.unknown.load(Ordering::Acquire) {
      ctl_ipc::write_frame(
        &mut *stream,
        &ServerMessage::Error {
          code: "ssh_status_unknown".into(),
          message: "could not observe the OpenSSH control master: control command timed out".into(),
        },
      )
      .await
      .unwrap();
      observations.failed.fetch_add(1, Ordering::Release);
      return false;
    }
  } else {
    observations.ensures.fetch_add(1, Ordering::Release);
    if interactive {
      observations.interactive.fetch_add(1, Ordering::Release);
      changed.notify_one();
    }
  }
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
        kind: if prompt == 2 {
          PromptKind::Confirm
        } else {
          PromptKind::Secret
        },
        message: format!("{PROMPT_TEXT}_{prompt}"),
        warning: Some(WARNING_TEXT.into()),
      },
    )
    .await
    .unwrap();
    let response = ctl_ipc::read_frame::<_, ClientMessage>(&mut *stream)
      .await
      .unwrap();
    if interactive {
      return answer_interactive_prompt(stream, response, prompt, prompt_mode, observations).await;
    }
    assert!(
      matches!(response, Some(ClientMessage::PromptResponse { prompt_id, response })
        if prompt_id == "fixture-prompt" && response.is_none()),
      "automatic recovery must cancel authentication without reading the host terminal"
    );
  }
  !passive && prompt != 0
}

async fn answer_interactive_prompt(
  stream: &mut tokio::net::UnixStream,
  response: Option<ClientMessage>,
  prompt: usize,
  prompt_mode: &AtomicUsize,
  observations: &Observations,
) -> bool {
  let Some(ClientMessage::PromptResponse {
    prompt_id,
    response: Some(response),
  }) = response
  else {
    return true;
  };
  assert_eq!(prompt_id, "fixture-prompt");
  let expected = if prompt == 2 { "yes" } else { SECRET_RESPONSE };
  assert_eq!(&*response, expected);
  let reply = if prompt == 3 {
    observations.rejected.fetch_add(1, Ordering::Release);
    ServerMessage::Error {
      code: "ssh_authentication_failed".into(),
      message: "Fixture authentication rejected".into(),
    }
  } else {
    observations.authenticated.fetch_add(1, Ordering::Release);
    prompt_mode.store(0, Ordering::Release);
    ServerMessage::MasterReady {
      control_path: PathBuf::from("/tmp/unused-fixture-master"),
    }
  };
  ctl_ipc::write_frame(stream, &reply).await.unwrap();
  false
}

fn footer(screen: &Screen) -> &str {
  screen.rows.last().map_or("", String::as_str)
}

async fn live_pane_survives_unknown_observations(
  tui: &mut Tui,
  broker: &Broker,
  proxy: &TestProxy,
) -> Result<()> {
  let attachments = proxy.attachments();
  // A failed observation must not tear down the healthy pane or block future
  // status checks. Existing work remains usable while passive status retries.
  broker.observations.unknown.store(true, Ordering::Release);
  broker.wait_counter(&broker.observations.failed, 1).await?;
  tui
    .wait_screen("unknown observation notice with a live pane", |screen| {
      footer(screen).starts_with(" connected |")
        && footer(screen).contains("ssh_status_unknown")
        && footer(screen).contains("Connection check failed")
        && !footer(screen).contains("automatic retries stopped")
    })
    .await?;
  tui.send(b"during-observation-failure\r")?;
  tui
    .wait_screen("live input while the control check is unknown", |screen| {
      screen.contains("shell:during-observation-failure")
    })
    .await?;
  assert_eq!(proxy.attachments(), attachments);

  let checks = broker.observations.checks.load(Ordering::Acquire);
  broker.observations.unknown.store(false, Ordering::Release);
  broker
    .wait_counter(&broker.observations.checks, checks + 2)
    .await?;
  tui
    .wait_screen(
      "periodic refresh clears the observation failure",
      |screen| {
        footer(screen).starts_with(" connected |")
          && footer(screen).contains("ctmux [observation-recovery]")
          && !footer(screen).contains("ssh_status_unknown")
      },
    )
    .await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_master_observations_recover_without_authentication_or_session_reselection()
-> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session(
      "observation-recovery",
      "shell",
      ctmux_proto::TerminalSize {
        columns: 160,
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
  let mut tui = Tui::spawn(command, 160, 13)?;
  tui
    .wait_screen("initial shell and connected footer", |screen| {
      screen.contains("shell:ready") && footer(screen).starts_with(" connected |")
    })
    .await?;
  broker.wait_counter(&broker.watches, 1).await?;
  let attachments = proxy.attachments();
  let ensures = broker.observations.ensures.load(Ordering::Acquire);

  live_pane_survives_unknown_observations(&mut tui, &broker, &proxy).await?;
  assert_eq!(broker.observations.ensures.load(Ordering::Acquire), ensures);

  // Losing the pane during another inconclusive check must also recover. No
  // recovery authorization is emitted by this broker, and no picker is opened.
  let failed = broker.observations.failed.load(Ordering::Acquire);
  broker.observations.unknown.store(true, Ordering::Release);
  proxy.interrupt();
  broker
    .wait_counter(&broker.observations.failed, failed + 1)
    .await?;
  tui
    .wait_screen(
      "disconnected pane retries the unknown control check",
      |screen| {
        footer(screen).starts_with(" reconnecting |")
          && footer(screen).contains("ssh_status_unknown")
      },
    )
    .await?;
  broker.observations.unknown.store(false, Ordering::Release);
  proxy.resume();
  proxy.wait_attachments(attachments + 1).await?;
  tui
    .wait_screen("replacement pane is adopted by the shell", |screen| {
      footer(screen).starts_with(" connected |")
        && footer(screen).contains("ctmux [observation-recovery]")
        && !footer(screen).contains("ssh_status_unknown")
    })
    .await?;
  tui.send(b"after-observation-recovery\r")?;
  tui
    .wait_screen(
      "pane resumes automatically after the observation recovers",
      |screen| {
        screen.contains("shell:after-observation-recovery")
          && footer(screen).starts_with(" connected |")
          && !footer(screen).contains("ssh_status_unknown")
      },
    )
    .await?;
  assert_eq!(broker.observations.ensures.load(Ordering::Acquire), ensures);
  assert_eq!(broker.cancelled.load(Ordering::Acquire), 0);
  assert!(!tui.transcript_contains(PROMPT_TEXT.as_bytes()));
  assert!(!tui.transcript_contains(WARNING_TEXT.as_bytes()));
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  broker.wait_counter(&broker.closed_watches, 1).await?;
  drop(tui);
  drop(broker);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

async fn retry_blocked_terminal(tui: &mut Tui, use_picker: bool) -> Result<()> {
  if use_picker {
    tui.send(b"\x02s")?;
    tui
      .wait_screen("session picker for explicit reconnect", |screen| {
        screen.contains("broker-reconnect")
      })
      .await?;
  }
  tui.send(b"\r")?;
  Ok(())
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
        footer(screen).contains("authentication")
          && footer(screen).contains("Enter to reconnect")
          && screen.contains("shell:ready")
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
    // must stop opening broker sockets until an explicit retry.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let cancelled = broker.cancelled.load(Ordering::Acquire);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(broker.cancelled.load(Ordering::Acquire), cancelled);
    broker.prompt.store(0, Ordering::Release);
    proxy.resume();
    // Retain session-picker coverage, then reconnect the same view with Enter.
    retry_blocked_terminal(&mut tui, cycle == 0).await?;
    proxy.wait_attachments(attachments + 1).await?;
    tui
      .wait_screen("connected screen after cancelling prompts", |screen| {
        footer(screen).starts_with(" connected |")
          && footer(screen).contains("ctmux [broker-reconnect]")
          && !footer(screen).contains("Enter to reconnect")
          && screen.contains("shell:ready")
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
  broker.wait_counter(&broker.closed_watches, 3).await?;
  assert_eq!(broker.watches.load(Ordering::Acquire), 3);
  assert_eq!(broker.closed_watches.load(Ordering::Acquire), 3);
  drop(tui);
  drop(broker);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

fn broker_terminal(
  daemon: &TestDaemon,
  proxy: &TestProxy,
  socket: &std::path::Path,
  session: &str,
) -> Result<Tui> {
  let mut command = CommandBuilder::new(std::env::current_exe()?);
  command.args(["--ignored", "--exact", CHILD_TEST, "--nocapture"]);
  command.env_clear();
  command.env("HOME", daemon.directory.join("home"));
  command.env("PATH", "/usr/bin:/bin");
  command.env("TERM", "xterm-256color");
  command.env("LANG", "C.UTF-8");
  command.env("CTLD_SOCKET_PATH", socket);
  command.env("CTL_TUI_TEST_SOCKET_PATH", &proxy.socket);
  command.env("CTL_TUI_TEST_SESSION", session);
  command.cwd(&daemon.directory);
  Tui::spawn(command, 120, 13)
}

async fn block_authentication(
  tui: &mut Tui,
  broker: &Broker,
  proxy: &TestProxy,
  prompt: usize,
) -> Result<()> {
  let cancelled = broker.cancelled.load(Ordering::Acquire);
  broker.prompt.store(prompt, Ordering::Release);
  proxy.interrupt();
  broker
    .wait_cancelled(cancelled + 1)
    .await
    .map_err(|error| format!("quiet authentication block: {error}"))?;
  tui
    .wait_screen(
      "quiet recovery requests explicit authentication",
      |screen| {
        footer(screen).contains("authentication")
          && footer(screen).contains("Enter to reconnect")
          && !screen.contains(PROMPT_TEXT)
      },
    )
    .await?;
  Ok(())
}

async fn authenticate_from_terminal(tui: &mut Tui, broker: &Broker, prompt: usize) -> Result<()> {
  open_authentication_prompt(tui, broker, prompt).await?;
  let response = if prompt == 2 { "yes" } else { SECRET_RESPONSE };
  tui.send(format!("{response}\r").as_bytes())?;
  Ok(())
}

async fn open_authentication_prompt(tui: &mut Tui, broker: &Broker, prompt: usize) -> Result<()> {
  let attempts = broker.observations.interactive.load(Ordering::Acquire);
  tui.send(b"\r")?;
  broker
    .wait_counter(&broker.observations.interactive, attempts + 1)
    .await
    .map_err(|error| format!("interactive authentication attempt: {error}"))?;
  let message = format!("{PROMPT_TEXT}_{prompt}");
  tui
    .wait_screen("explicit reconnect authentication prompt", |screen| {
      screen.contains(&message) && screen.contains(WARNING_TEXT)
    })
    .await?;
  if prompt != 2 {
    timeout(Duration::from_secs(5), async {
      while tui.host_terminal_is_cooked()? {
        tokio::time::sleep(Duration::from_millis(5)).await;
      }
      Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await
    .map_err(|_| "password prompt did not disable terminal echo")??;
  }
  Ok(())
}

async fn verify_reconnected_shell(tui: &mut Tui, proxy: &TestProxy, cycle: usize) -> Result<()> {
  proxy.wait_attachments(cycle + 1).await?;
  tui
    .wait_screen("existing shell restored after authentication", |screen| {
      footer(screen).starts_with(" connected |")
        && footer(screen).contains("ctmux [explicit-authentication]")
        && screen.contains("shell:ready")
    })
    .await?;
  let marker = format!("authenticated-{cycle}");
  tui.send(format!("{marker}\r").as_bytes())?;
  tui
    .wait_screen("input returns to the original pane", |screen| {
      screen.contains(&format!("shell:{marker}"))
    })
    .await?;
  assert!(!tui.transcript_contains(SECRET_RESPONSE.as_bytes()));
  Ok(())
}

async fn authentication_fixture() -> Result<(TestDaemon, TestProxy, Broker, Tui)> {
  let daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session(
      "explicit-authentication",
      "shell",
      ctmux_proto::TerminalSize {
        columns: 120,
        rows: 12,
        ..Default::default()
      },
    )
    .await?;
  let proxy = TestProxy::start(&daemon)?;
  let broker = Broker::start(&daemon.directory.join("broker.sock"))?;
  let mut tui = broker_terminal(
    &daemon,
    &proxy,
    &daemon.directory.join("broker.sock"),
    &session,
  )?;
  tui
    .wait_screen("initial shell", |screen| {
      screen.contains("shell:ready") && footer(screen).starts_with(" connected |")
    })
    .await?;
  Ok((daemon, proxy, broker, tui))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enter_authenticates_the_existing_shell_and_restores_ui_after_rejection() -> Result<()> {
  let (mut daemon, proxy, broker, mut tui) = authentication_fixture().await?;
  block_authentication(&mut tui, &broker, &proxy, 3).await?;
  assert!(!tui.transcript_contains(PROMPT_TEXT.as_bytes()));
  assert!(!tui.transcript_contains(WARNING_TEXT.as_bytes()));
  proxy.resume();
  authenticate_from_terminal(&mut tui, &broker, 3).await?;
  broker
    .wait_counter(&broker.observations.rejected, 1)
    .await
    .map_err(|error| format!("authentication rejection: {error}"))?;
  tui
    .wait_screen("authentication rejection returns to the TUI", |screen| {
      footer(screen).contains("Enter to reconnect")
        && footer(screen).contains("Fixture authentication rejected")
        && screen.contains("shell:ready")
    })
    .await?;
  assert!(!tui.transcript_contains(SECRET_RESPONSE.as_bytes()));
  let ensures = broker.observations.ensures.load(Ordering::Acquire);
  tokio::time::sleep(Duration::from_secs(3)).await;
  assert_eq!(broker.observations.ensures.load(Ordering::Acquire), ensures);
  assert_eq!(proxy.attachments(), 1);

  broker.prompt.store(1, Ordering::Release);
  authenticate_from_terminal(&mut tui, &broker, 1).await?;
  broker
    .wait_counter(&broker.observations.authenticated, 1)
    .await
    .map_err(|error| format!("secret authentication: {error}"))?;
  verify_reconnected_shell(&mut tui, &proxy, 1).await?;
  block_authentication(&mut tui, &broker, &proxy, 2).await?;
  proxy.resume();
  authenticate_from_terminal(&mut tui, &broker, 2).await?;
  broker
    .wait_counter(&broker.observations.authenticated, 2)
    .await
    .map_err(|error| format!("confirmation authentication: {error}"))?;
  verify_reconnected_shell(&mut tui, &proxy, 2).await?;

  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  let watches = broker.watches.load(Ordering::Acquire);
  broker
    .wait_counter(&broker.closed_watches, watches)
    .await
    .map_err(|error| format!("recovery watch cleanup: {error}"))?;
  drop(tui);
  drop(broker);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_secret_prompt_restores_the_disconnected_ui_for_another_retry() -> Result<()> {
  let (mut daemon, proxy, broker, mut tui) = authentication_fixture().await?;
  block_authentication(&mut tui, &broker, &proxy, 1).await?;
  proxy.resume();
  open_authentication_prompt(&mut tui, &broker, 1).await?;
  tui.send(b"\x04")?;
  tui
    .wait_screen("cancelled prompt returns to the TUI", |screen| {
      footer(screen).contains("Enter to reconnect")
        && footer(screen).contains("unexpected end of file")
        && screen.contains("shell:ready")
    })
    .await?;
  assert_eq!(proxy.attachments(), 1);
  authenticate_from_terminal(&mut tui, &broker, 1).await?;
  broker
    .wait_counter(&broker.observations.authenticated, 1)
    .await?;
  verify_reconnected_shell(&mut tui, &proxy, 1).await?;
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  drop(broker);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigterm_during_a_secret_prompt_restores_cooked_terminal_modes() -> Result<()> {
  use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
  };
  let (mut daemon, proxy, broker, mut tui) = authentication_fixture().await?;
  block_authentication(&mut tui, &broker, &proxy, 1).await?;
  proxy.resume();
  open_authentication_prompt(&mut tui, &broker, 1).await?;
  assert!(!tui.host_terminal_is_cooked()?);
  let process_id = i32::try_from(tui.process_id().ok_or("TUI has no process ID")?)?;
  kill(Pid::from_raw(process_id), Signal::SIGTERM)?;
  assert!(tui.wait_exit().await?.success());
  assert!(tui.host_terminal_is_cooked()?);
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
  // Match main's process exit instead of waiting for a canceled blocking
  // password worker during the private test runtime's shutdown.
  std::process::exit(0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shell_resume_opens_the_picker_before_attaching_or_creating() -> Result<()> {
  check_resume_picker("shell").await?;
  check_resume_picker("ctmux").await
}

async fn check_resume_picker(entrypoint: &str) -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session(
      "existing-work",
      "resumed",
      ctmux_proto::TerminalSize {
        columns: 120,
        rows: 12,
        ..Default::default()
      },
    )
    .await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut command = CommandBuilder::new(std::env::current_exe()?);
  command.args(["--ignored", "--exact", RESUME_CHILD_TEST, "--nocapture"]);
  command.env_clear();
  command.env("HOME", daemon.directory.join("home"));
  command.env("PATH", "/usr/bin:/bin");
  command.env("TERM", "xterm-256color");
  command.env("LANG", "C.UTF-8");
  command.env("CTL_TUI_TEST_SOCKET_PATH", &proxy.socket);
  command.env("CTL_TUI_TEST_ENTRYPOINT", entrypoint);
  command.cwd(&daemon.directory);
  let mut tui = Tui::spawn(command, 120, 13)?;
  tui
    .wait_screen("existing-session startup picker", |screen| {
      screen.contains("existing-work")
    })
    .await?;
  assert_eq!(proxy.attachments(), 0, "resume must wait for a selection");
  let ctmux_proto::ServerMessage::SessionList { sessions } = daemon
    .request(ctmux_proto::ClientMessage::ListSessions)
    .await?
  else {
    return Err("expected session list".into());
  };
  assert_eq!(sessions.len(), 1, "resume must not create an extra session");
  assert_eq!(sessions[0].session_id, session);
  tui.send(b"\r")?;
  tui
    .wait_screen("selected persistent shell", |screen| {
      screen.contains("resumed:ready") && footer(screen).starts_with(" connected |")
    })
    .await?;
  proxy.wait_attachments(1).await?;
  tui.send(b"still-running\r")?;
  tui
    .wait_screen("input goes to the resumed shell", |screen| {
      screen.contains("resumed:still-running")
    })
    .await?;
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test]
#[ignore = "private real-terminal subprocess fixture"]
async fn resume_terminal_fixture() {
  let socket = std::env::var_os("CTL_TUI_TEST_SOCKET_PATH").expect("fixture socket");
  let connector = CtlConnector::new(
    ConnectionTarget::Local {
      socket_path: socket.into(),
    },
    ctl_client::hosts::ConnectionTargetDto::Local,
  );
  let entrypoint = std::env::var("CTL_TUI_TEST_ENTRYPOINT").expect("fixture entrypoint");
  let arguments = Arguments::try_parse_from(["ctl", &entrypoint, "--resume"]).unwrap();
  run_selected(
    arguments.command,
    &connector,
    ctl_client::RemotePlatform::Unix,
  )
  .await
  .unwrap();
  assert!(!connector.terminal_ui_active.load(Ordering::Acquire));
}
