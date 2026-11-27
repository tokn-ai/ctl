//! An acknowledged native input call can wait on SSH, the host or the OS.
//! Renderers only exchange bounded work and status with this worker.
use super::{Forwarder, Sink};
use crate::view;
use ctream_client::{geometry::ImageGeometry, transport::Cancellation};
use eframe::egui::{self, Event, Modifiers};
use std::{
  collections::VecDeque,
  io,
  sync::{Arc, Condvar, Mutex},
};

const LIMIT: usize = 64;

struct Events {
  image: ImageGeometry,
  regions: Vec<egui::Rect>,
  events: Vec<Event>,
  modifiers: Modifiers,
}

impl Events {
  fn motion(&self) -> bool {
    matches!(&self.events[..], [Event::PointerMoved(_)])
  }
}

enum Job {
  Input(Events),
  Reset,
  Toggle,
}

impl Job {
  fn cost(&self) -> usize {
    match self {
      Self::Input(input) => input.events.len().max(1),
      _ => 1,
    }
  }
}

#[derive(Default)]
struct Queue {
  jobs: VecDeque<Job>,
  count: usize,
  error: Option<io::Error>,
  delivered: bool,
  turn: bool,
  toggling: bool,
  closed: bool,
}

pub(crate) struct Worker {
  queue: Arc<(Mutex<Queue>, Condvar)>,
  cancel: Cancellation,
  pub(super) size: Option<[u32; 2]>,
  pub(super) forwarding: bool,
  pub(super) delivered: bool,
  modifiers: Modifiers,
}

impl Worker {
  pub(super) fn start<S: Sink + Send + 'static>(
    mut input: Forwarder<S>,
    cancel: Cancellation,
    ctx: egui::Context,
  ) -> Self {
    let size = input.desktop_size();
    let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
    let shared = Arc::clone(&queue);
    std::thread::spawn(move || {
      let (lock, changed) = &*shared;
      loop {
        let job = {
          let mut q = lock.lock().unwrap();
          while !q.closed && q.jobs.is_empty() {
            q = changed.wait(q).unwrap();
          }
          if q.closed {
            break;
          }
          let job = q.jobs.pop_front().unwrap();
          q.count -= job.cost();
          job
        };
        input.delivered = false;
        let toggle = matches!(job, Job::Toggle);
        let response = match job {
          Job::Input(e) => {
            input.forward_while(&e.image, &e.events, e.modifiers, &e.regions, || {
              !lock.lock().unwrap().closed
            })
          }
          Job::Reset => input.reset(),
          Job::Toggle => {
            // A toolbar action explicitly ends any previous native gesture.
            // Flush its release before the host grants or releases a turn.
            if input.turn_state() == Some(true) {
              // The release request itself drains OS input, including after
              // the watchdog has already expired this token.
              input.toggle_turn().map(|()| input.clear_gesture())
            } else if input.holding() {
              input.reset().and_then(|()| input.toggle_turn())
            } else {
              input.toggle_turn()
            }
          }
        };
        let mut q = lock.lock().unwrap();
        q.turn = input.turn_state().unwrap_or(false);
        if toggle {
          q.toggling = false;
        }
        match response {
          Ok(()) => q.delivered |= input.delivered || toggle,
          Err(err) => {
            if err
              .get_ref()
              .and_then(|e| e.downcast_ref::<ctream_proto::live::TurnError>())
              .is_some_and(|e| e.code == "stale_turn")
            {
              // The host watchdog already released old OS edges.
              input.clear_gesture();
            }
            q.closed |= err.kind() != io::ErrorKind::WouldBlock;
            // None of these queued gestures has been dispatched. Releasing a
            // competitor's turn must not make our declined clicks execute later.
            q.jobs.clear();
            q.count = 0;
            q.toggling = false;
            q.delivered = false;
            q.error = Some(err);
          }
        }
        drop(q);
        ctx.request_repaint_of(view::viewer_id());
      }
    });
    Self {
      queue,
      cancel,
      size,
      forwarding: false,
      delivered: false,
      modifiers: Modifiers::NONE,
    }
  }

  pub(super) fn turn(&self) -> bool {
    self.queue.0.lock().unwrap().turn
  }

  fn push(&self, job: Job) -> io::Result<()> {
    let (lock, changed) = &*self.queue;
    let mut q = lock.lock().unwrap();
    if q.closed || q.error.is_some() {
      return Ok(());
    }
    if let Job::Input(next) = &job
      && next.motion()
      && let Some(Job::Input(last)) = q.jobs.back_mut()
      && last.motion()
      && last.modifiers == next.modifiers
    {
      *q.jobs.back_mut().unwrap() = job;
      return Ok(());
    }
    if job.cost() + q.count > LIMIT {
      q.closed = true;
      q.jobs.clear();
      drop(q);
      changed.notify_one();
      self.cancel.cancel();
      return Err(io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "input stopped because the host could not keep up",
      ));
    }
    if matches!(job, Job::Toggle) {
      if q.toggling {
        return Ok(());
      }
      q.toggling = true;
    }
    q.count += job.cost();
    q.jobs.push_back(job);
    changed.notify_one();
    Ok(())
  }

  pub(super) fn toggle(&self) -> io::Result<()> {
    self.push(Job::Toggle)
  }

  pub(super) fn update(
    &mut self,
    ctx: &egui::Context,
    image: Option<&ImageGeometry>,
    regions: &[egui::Rect],
  ) -> io::Result<()> {
    self.delivered = false;
    if ctx.current_pass_index() != 0 {
      return Ok(());
    }
    {
      let mut q = self.queue.0.lock().unwrap();
      if let Some(err) = q.error.take() {
        return Err(err);
      }
      self.delivered = std::mem::take(&mut q.delivered);
    }
    let Some(image) =
      image.filter(|_| ctx.input(|i| i.focused) && !ctx.egui_wants_keyboard_input())
    else {
      if std::mem::take(&mut self.forwarding) {
        self.modifiers = Modifiers::NONE;
        self.push(Job::Reset)?;
      }
      return Ok(());
    };
    self.forwarding = true;
    let (events, mods) = ctx.input(|i| {
      let mut events = Vec::new();
      for e in &i.raw.events {
        let event = match e {
          Event::PointerMoved(_) if matches!(events.last(), Some(Event::PointerMoved(_))) => {
            *events.last_mut().unwrap() = e.clone();
            continue;
          }
          Event::PointerMoved(_)
          | Event::PointerGone
          | Event::PointerButton { .. }
          | Event::MouseWheel { .. }
          | Event::Key { .. }
          | Event::Copy
          | Event::Cut => e.clone(),
          Event::Paste(_) => Event::Paste(String::new()),
          _ => continue,
        };
        events.push(event);
        if events.len() > LIMIT {
          break;
        }
      }
      (events, i.modifiers)
    });
    if !events.is_empty() || self.modifiers != mods {
      self.modifiers = mods;
      self.push(Job::Input(Events {
        image: *image,
        regions: regions.to_vec(),
        events,
        modifiers: mods,
      }))?;
    }
    Ok(())
  }
}

impl Drop for Worker {
  fn drop(&mut self) {
    let mut q = self.queue.0.lock().unwrap();
    q.closed = true;
    q.jobs.clear();
    drop(q);
    self.queue.1.notify_one();
    self.cancel.cancel();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::input::Input;
  use ctream_client::{
    input::{Input as NativeInput, InputFormat},
    transport::Channel,
  };
  use std::{
    io::{BufRead, BufReader, Write},
    sync::mpsc,
    time::{Duration, Instant},
  };

  #[test]
  fn a_stalled_turn_reply_allows_rendering_coalesces_motion_and_never_replays_after_busy() {
    let (incoming, writer) = io::pipe().unwrap();
    let (reader, mut reply) = io::pipe().unwrap();
    reply.write_all(b"ready\n").unwrap();
    let response = Arc::new(Mutex::new(Some(reply)));
    let closing = Arc::clone(&response);
    let control = NativeInput::open_viewer(
      Channel {
        reader,
        writer,
        cancellation: Cancellation::new(move || {
          closing.lock().unwrap().take();
        }),
      },
      InputFormat::Macos,
    )
    .unwrap();
    let ctx = egui::Context::default();
    let mut input = Input::new(control, Default::default(), &ctx);
    let (requests, seen) = mpsc::channel();
    let (allow, wait) = mpsc::channel();
    let server = std::thread::spawn(move || {
      let mut incoming = BufReader::new(incoming);
      let mut line = String::new();
      incoming.read_line(&mut line).unwrap();
      requests.send(line).unwrap();
      wait.recv_timeout(Duration::from_secs(4)).unwrap();
      response
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .write_all(b"{\"error\":{\"code\":\"busy\",\"owner_id\":\"owner\"}}\n")
        .unwrap();
      let mut line = String::new();
      incoming.read_line(&mut line).unwrap();
      requests.send(line).unwrap();
    });
    input.toggle_turn().unwrap();
    assert_eq!(
      seen.recv_timeout(Duration::from_secs(2)).unwrap(),
      "take-turn 10000\n"
    );
    let image = crate::view::fit(
      egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320., 200.)),
      [320, 200],
    )
    .unwrap();
    let frame = |input: &mut Input<_>, events| {
      let mut result = Ok(());
      let raw = egui::RawInput {
        events,
        focused: true,
        ..Default::default()
      };
      let mut output = ctx.run_ui(raw, |ui| {
        ui.label("Capture is still repainting");
        result = input.update(ui.ctx(), Some(&image), &[]);
      });
      assert!(!output.shapes.is_empty());
      output.textures_delta.clear();
      result
    };
    // The host still has not answered the turn request. All of these viewport
    // passes must finish, even when considerably more than LIMIT events arrive.
    for x in 0..LIMIT * 3 {
      frame(
        &mut input,
        vec![Event::PointerMoved(egui::pos2(x as f32, 60.))],
      )
      .unwrap();
    }
    allow.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
      match frame(&mut input, Vec::new()) {
        Err(e) => {
          assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
          assert!(e.to_string().contains("owner"));
          break;
        }
        Ok(()) => {
          assert!(Instant::now() < deadline);
          std::thread::yield_now();
        }
      }
    }
    drop(input);
    assert_eq!(
      seen.recv_timeout(Duration::from_secs(2)).unwrap(),
      "",
      "queued motion must never reach the backend after busy"
    );
    server.join().unwrap();
  }
}
