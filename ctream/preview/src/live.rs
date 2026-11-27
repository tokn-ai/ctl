//! Optional, source-scoped activity. The input and capture paths never wait for a subscriber.
mod overlay;

use crate::view;
use ctream_client::{
  Session,
  geometry::ImageGeometry,
  live::ViewerAttachment,
  transport::{Cancellation, Transport},
};
use ctream_proto::live::LiveAction;
use eframe::egui;
use std::{
  cell::Cell,
  collections::VecDeque,
  sync::{
    Arc, Condvar, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
  },
  thread,
  time::{Duration, Instant},
};

const MAX_PENDING: usize = 64;
const INITIAL_DELAY: Duration = Duration::from_millis(250);
const MAX_DELAY: Duration = Duration::from_secs(15);

#[derive(Default)]
struct Queue {
  actions: VecDeque<LiveAction>,
  joined: bool,
  closed: bool,
}

/// Only one connection can consume these actions. Reconnecting always starts with an empty queue.
struct Outbox {
  queue: Mutex<Queue>,
  wake: Condvar,
  cancellation: Cancellation,
}

impl Outbox {
  fn new(cancellation: Cancellation) -> Self {
    Self {
      queue: Mutex::default(),
      wake: Condvar::new(),
      cancellation,
    }
  }

  fn next(&self) -> Option<LiveAction> {
    let queue = self.queue.lock().unwrap();
    let mut queue = self
      .wake
      .wait_while(queue, |q| !q.closed && q.actions.is_empty())
      .unwrap();
    queue.actions.pop_front()
  }

  fn close(&self) {
    let mut queue = self.queue.lock().unwrap();
    queue.closed = true;
    queue.actions.clear();
    drop(queue);
    self.wake.notify_all();
    self.cancellation.cancel();
  }

  fn push(&self, action: LiveAction) {
    let mut queue = self.queue.lock().unwrap();
    if !queue.joined || queue.closed {
      return;
    }
    let movement = matches!(action, LiveAction::Move { .. });
    if movement && matches!(queue.actions.back(), Some(LiveAction::Move { .. })) {
      queue.actions.pop_back();
    }
    if queue.actions.len() == MAX_PENDING {
      if let Some(old) = queue
        .actions
        .iter()
        .position(|a| matches!(a, LiveAction::Move { .. }))
      {
        queue.actions.remove(old);
      } else if movement {
        return;
      } else {
        // Losing a press/release could leave peers showing a gesture that never
        // ends. Leaving the room retires that participant and its entire trail.
        drop(queue);
        self.close();
        return;
      }
    }
    queue.actions.push_back(action);
    self.wake.notify_one();
  }
}

#[derive(Default)]
struct Lifecycle {
  active: Option<Arc<Outbox>>,
  stopped: bool,
  binding: Binding,
}

#[derive(Default)]
enum Binding {
  #[default]
  Connecting,
  Standalone,
  Attached(ViewerAttachment),
  Failed(String),
}

#[derive(Default)]
struct Subscription {
  state: Mutex<Lifecycle>,
  wake: Condvar,
}

impl Subscription {
  fn binding(&self, binding: Binding) {
    self.state.lock().unwrap().binding = binding;
    self.wake.notify_all();
  }

  fn attach(&self, outbox: Arc<Outbox>) -> bool {
    let mut state = self.state.lock().unwrap();
    if state.stopped {
      return false;
    }
    state.active = Some(outbox);
    true
  }

  fn wait_for_retry(&self, delay: Duration) -> bool {
    let state = self.state.lock().unwrap();
    let (state, _) = self
      .wake
      .wait_timeout_while(state, delay, |s| !s.stopped)
      .unwrap();
    !state.stopped
  }

  fn stop(&self) {
    let mut state = self.state.lock().unwrap();
    state.stopped = true;
    let active = state.active.take();
    drop(state);
    self.wake.notify_all();
    if let Some(active) = active {
      active.close();
    }
  }
}

#[derive(Clone, Default)]
pub(super) struct Publisher(Option<Weak<Subscription>>);

impl Publisher {
  /// Called on control workers. An older host must explicitly decline pairing
  /// before a viewer can open independent input.
  pub(super) fn attachment(&self) -> std::io::Result<Option<ViewerAttachment>> {
    let Some(source) = &self.0 else {
      return Ok(None);
    };
    let subscription = source.upgrade().ok_or(std::io::ErrorKind::Interrupted)?;
    let state = subscription.state.lock().unwrap();
    let state = subscription
      .wake
      .wait_while(state, |s| {
        !s.stopped && matches!(s.binding, Binding::Connecting)
      })
      .unwrap();
    if state.stopped {
      return Err(std::io::ErrorKind::Interrupted.into());
    }
    match &state.binding {
      Binding::Attached(proof) => Ok(Some(proof.clone())),
      Binding::Standalone => Ok(None),
      Binding::Failed(reason) => Err(std::io::Error::other(reason.clone())),
      Binding::Connecting => unreachable!(),
    }
  }

  pub(super) fn send(&self, action: LiveAction) {
    if let Some(subscription) = self.0.as_ref().and_then(Weak::upgrade) {
      let active = subscription.state.lock().unwrap().active.clone();
      if let Some(active) = active {
        active.push(action);
      }
    }
  }
}

pub(super) fn viewer_id() -> &'static str {
  static VIEWER_ID: OnceLock<String> = OnceLock::new();
  VIEWER_ID.get_or_init(ctream_client::generate_agent_id)
}

pub(super) struct Live {
  subscription: Arc<Subscription>,
  state: Arc<Mutex<overlay::Roster>>,
  dirty: Arc<AtomicBool>,
  pub(super) region: Cell<egui::Rect>,
}

#[derive(Clone, Copy)]
enum RoomProtocol {
  Daemon,
  Turns,
  Overlay,
}

/// A host may report an unsupported contract either on opening the channel or
/// at the first read from the child. Negotiate only before a roster was received.
fn visit_room<T: Transport>(
  session: &Session<T>,
  label: &str,
  protocol: RoomProtocol,
  subscription: &Subscription,
  state: &Mutex<overlay::Roster>,
  refresh: &impl Fn(),
) -> std::io::Result<Duration> {
  let channel = match protocol {
    RoomProtocol::Daemon => session.viewer_observer(label),
    RoomProtocol::Turns => session.live_with_turns(label),
    RoomProtocol::Overlay => session.live(label),
  }?;
  let outbox = Arc::new(Outbox::new(channel.cancellation()));
  if !subscription.attach(outbox.clone()) {
    return Err(std::io::ErrorKind::Interrupted.into());
  }
  let (mut reader, mut writer) = channel.split();
  let result = (|| {
    let attachment = if matches!(protocol, RoomProtocol::Daemon) {
      Some(reader.viewer_attachment()?)
    } else {
      None
    };
    let snapshot = reader
      .next_event()?
      .ok_or(std::io::ErrorKind::UnexpectedEof)?;
    let joined = Instant::now();
    if let Some(attachment) = attachment {
      subscription.binding(Binding::Attached(attachment));
    }
    state.lock().unwrap().apply(snapshot, joined);
    refresh();
    // A daemon room only receives host events. Keep the writer attached for
    // its connection lifetime; older rooms additionally accept viewer hints.
    let _receive_only = match protocol {
      RoomProtocol::Daemon => Some(writer),
      _ => {
        outbox.queue.lock().unwrap().joined = true;
        let sending = outbox.clone();
        thread::spawn(move || {
          while let Some(action) = sending.next() {
            if writer.publish(&action).is_err() {
              break;
            }
          }
          sending.close();
        });
        None
      }
    };
    while let Ok(Some(event)) = reader.next_event() {
      state.lock().unwrap().apply(event, Instant::now());
      refresh();
    }
    Ok(joined.elapsed())
  })();
  outbox.close();
  subscription.state.lock().unwrap().active.take();
  if matches!(protocol, RoomProtocol::Daemon) {
    subscription.binding(Binding::Connecting);
  }
  result
}

impl Live {
  pub(super) fn open<T: Transport + Send + Sync + 'static>(
    session: Session<T>,
    ctx: &egui::Context,
    repaint: view::Repaint,
  ) -> Self {
    let live = Self {
      subscription: Arc::default(),
      state: Arc::default(),
      dirty: Arc::default(),
      region: Cell::new(egui::Rect::NOTHING),
    };
    let subscription = live.subscription.clone();
    let state = live.state.clone();
    let dirty = live.dirty.clone();
    let pending = dirty.clone();
    let wake = view::capture_repaint(ctx, repaint, move || {
      view::PendingCapture::new(pending.load(Ordering::Relaxed), false)
    });
    thread::spawn(move || {
      let refresh = || {
        dirty.store(true, Ordering::Relaxed);
        let _ = wake.try_send(());
      };
      let label = viewer_id();
      let mut joined_before = false;
      let mut delay = INITIAL_DELAY;
      while !subscription.state.lock().unwrap().stopped {
        for protocol in [
          RoomProtocol::Daemon,
          RoomProtocol::Turns,
          RoomProtocol::Overlay,
        ] {
          let visit = visit_room(&session, label, protocol, &subscription, &state, &refresh);
          if let Err(error) = &visit
            && ctream_client::transport::unsupported_operation(error)
          {
            if matches!(protocol, RoomProtocol::Daemon) {
              subscription.binding(Binding::Standalone);
            }
            continue;
          }
          if let Err(error) = &visit
            && matches!(protocol, RoomProtocol::Daemon)
            && !joined_before
          {
            subscription.binding(Binding::Failed(error.to_string()));
          }
          if let Ok(connected) = visit {
            joined_before = true;
            if connected >= MAX_DELAY {
              delay = INITIAL_DELAY;
            }
          }
          break;
        }
        // An old host never joined the first room. Capture continues normally.
        if !joined_before {
          break;
        }
        *state.lock().unwrap() = overlay::Roster::reconnecting();
        refresh();
        if !subscription.wait_for_retry(delay) {
          break;
        }
        delay = (delay * 2).min(MAX_DELAY);
      }
    });
    live
  }

  pub(super) fn publisher(&self) -> Publisher {
    Publisher(Some(Arc::downgrade(&self.subscription)))
  }

  pub(super) fn paint(&self, ui: &mut egui::Ui, image: &ImageGeometry) {
    self.dirty.store(false, Ordering::Relaxed);
    self
      .region
      .set(self.state.lock().unwrap().paint(ui, image, Instant::now()));
  }
}

impl Drop for Live {
  fn drop(&mut self) {
    self.subscription.stop();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn slow_observers_keep_edges_or_leave_and_discard_the_entire_queue() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let observed = cancelled.clone();
    let queue = Outbox::new(Cancellation::new(move || {
      observed.store(true, Ordering::SeqCst)
    }));
    queue.queue.lock().unwrap().joined = true;
    for n in 0..100 {
      queue.push(LiveAction::Move {
        point: [n as f32 / 100.0, 0.0],
      });
    }
    assert_eq!(queue.queue.lock().unwrap().actions.len(), 1);
    for _ in 0..MAX_PENDING {
      queue.push(LiveAction::PressKeys {});
    }
    queue.push(LiveAction::Move { point: [0.5, 0.5] });
    assert!(!cancelled.load(Ordering::SeqCst));
    assert!(
      queue
        .queue
        .lock()
        .unwrap()
        .actions
        .iter()
        .all(|a| matches!(a, LiveAction::PressKeys {}))
    );
    queue.push(LiveAction::PointerUp { point: [0.5, 0.5] });
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(queue.next().is_none());
  }
}
