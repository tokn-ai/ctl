use crate::Result;
use std::{
  cell::RefCell,
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
  },
  thread::{self, JoinHandle},
  time::Duration,
};
use windows::Win32::{
  Foundation::HWND,
  UI::{
    Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
    WindowsAndMessaging::{
      DispatchMessageW, EVENT_OBJECT_DESTROY, MSG, OBJID_WINDOW, PM_REMOVE, PeekMessageW,
      WINEVENT_OUTOFCONTEXT,
    },
  },
};

struct WatchedWindow {
  handle: usize,
  dead: Arc<AtomicBool>,
}

thread_local! {
  static WATCHED: RefCell<Option<WatchedWindow>> = const { RefCell::new(None) };
}

unsafe extern "system" fn destroyed(
  _: HWINEVENTHOOK,
  _: u32,
  hwnd: HWND,
  object: i32,
  child: i32,
  _: u32,
  _: u32,
) {
  if object != OBJID_WINDOW.0 || child != 0 {
    return;
  }
  WATCHED.with(|state| {
    if let Some(target) = state.borrow().as_ref()
      && target.handle == hwnd.0 as usize
    {
      target.dead.store(true, Ordering::Release);
    }
  });
}

/// Observed destroy events permanently invalidate an open target. Out-of-context
/// delivery is asynchronous, so this is not an atomic guarantee against HWND reuse.
pub(crate) struct WindowWatch {
  dead: Arc<AtomicBool>,
  stop: Arc<AtomicBool>,
  worker: Option<JoinHandle<()>>,
}

impl WindowWatch {
  pub fn new(handle: usize, process: u32, window_thread: u32) -> Result<Self> {
    let dead = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let (ready, started) = mpsc::sync_channel(1);
    let worker_dead = dead.clone();
    let worker_stop = stop.clone();
    let worker = thread::Builder::new()
      .name("ctream-window-watch".into())
      .spawn(move || {
        WATCHED.with(|state| {
          *state.borrow_mut() = Some(WatchedWindow {
            handle,
            dead: worker_dead.clone(),
          })
        });
        let hook = unsafe {
          SetWinEventHook(
            EVENT_OBJECT_DESTROY,
            EVENT_OBJECT_DESTROY,
            None,
            Some(destroyed),
            process,
            window_thread,
            WINEVENT_OUTOFCONTEXT,
          )
        };
        if hook.0.is_null() {
          let _ = ready.send(Err(windows::core::Error::from_thread().to_string()));
          return;
        }
        let _ = ready.send(Ok(()));
        while !worker_stop.load(Ordering::Acquire) {
          let mut message = MSG::default();
          while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe { DispatchMessageW(&message) };
          }
          thread::sleep(Duration::from_millis(10));
        }
        let _ = unsafe { UnhookWinEvent(hook) };
        WATCHED.with(|state| state.borrow_mut().take());
        worker_dead.store(true, Ordering::Release);
      })?;
    let watch = Self {
      dead,
      stop,
      worker: Some(worker),
    };
    started
      .recv()?
      .map_err(|error| format!("Windows window lifetime tracking failed: {error}"))?;
    Ok(watch)
  }

  pub fn dead(&self) -> bool {
    self.dead.load(Ordering::Acquire)
  }
}

impl Drop for WindowWatch {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::Release);
    if let Some(worker) = self.worker.take() {
      let _ = worker.join();
    }
  }
}
