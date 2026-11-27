//! Window capture from the macOS content-sharing picker or window catalog.

use ctream_driver::CaptureOptions;
use std::{
  ffi::{CStr, c_char, c_void},
  io,
  marker::PhantomData,
  panic::{AssertUnwindSafe, catch_unwind},
  ptr::{self, NonNull},
  rc::Rc,
  slice,
};

pub const DEFAULT_MAX_FPS: u16 = 30;

pub enum Event {
  Frame { size: [usize; 2], pixels: Vec<u8> },
  Cancelled,
  Finished,
  Failed(String),
}

struct Callback(Box<dyn Fn(Event) + Send + Sync>);

/// A window from one discovery snapshot. Capture starts with this retained window.
pub struct Window {
  pub id: u32,
  pub pid: i32,
  pub title: String,
  pub application: String,
  native: NativeWindow,
}

struct NativeWindow(NonNull<c_void>);

// The reference can move between threads. The native bridge confines opening
// and releasing the retained ScreenCaptureKit object to the main thread.
unsafe impl Send for NativeWindow {}

impl Drop for NativeWindow {
  fn drop(&mut self) {
    unsafe { ctream_sck_release_window(self.0.as_ptr()) };
  }
}

type ListCallback = Box<dyn FnOnce(io::Result<Vec<Window>>) + Send>;

struct WindowList {
  windows: Vec<Window>,
  callback: ListCallback,
}

/// Starts a window query on the main thread using existing Screen Recording
/// permission. The callback runs on the main thread when the query finishes.
pub fn list_windows(
  callback: impl FnOnce(io::Result<Vec<Window>>) + Send + 'static,
) -> io::Result<()> {
  let list = Box::into_raw(Box::new(WindowList {
    windows: Vec::new(),
    callback: Box::new(callback),
  }));
  let status = unsafe { ctream_sck_list_windows(list.cast(), listed_window, finish_list) };
  if status == 0 {
    return Ok(());
  }
  // A rejected query does not transfer context ownership to the bridge.
  unsafe { drop(Box::from_raw(list)) };
  Err(native_error(status))
}

/// Owns one window capture, including its system picker when used for selection.
pub struct Capture {
  native: NonNull<c_void>,
  main_thread: PhantomData<Rc<()>>,
}

impl Capture {
  /// The system picker is available on macOS 14 and later.
  pub fn is_supported() -> bool {
    unsafe { ctream_sck_is_supported() != 0 }
  }

  /// Checks existing system authorization without prompting for it.
  pub fn has_screen_recording_access() -> bool {
    unsafe { ctream_sck_has_screen_recording_access() != 0 }
  }

  /// Opens the native picker on the main thread. Frame events may arrive on
  /// the capture queue, so the callback must be thread-safe and nonblocking.
  pub fn start(callback: impl Fn(Event) + Send + Sync + 'static) -> io::Result<Self> {
    Self::start_source(ptr::null_mut(), i32::from(DEFAULT_MAX_FPS), false, callback)
  }

  /// Captures the selected window from a discovery snapshot on the main thread.
  pub fn start_window(
    window: &Window,
    callback: impl Fn(Event) + Send + Sync + 'static,
  ) -> io::Result<Self> {
    Self::start_source(
      window.native.0.as_ptr(),
      i32::from(DEFAULT_MAX_FPS),
      false,
      callback,
    )
  }

  /// Capture a directly selected window for an independently leased session.
  /// The host can share this stream across independently leased subscribers.
  pub fn start_session_window(
    window: &Window,
    callback: impl Fn(Event) + Send + Sync + 'static,
  ) -> io::Result<Self> {
    Self::start_source(
      window.native.0.as_ptr(),
      i32::from(DEFAULT_MAX_FPS),
      true,
      callback,
    )
  }

  pub fn start_session_window_with_options(
    window: &Window,
    options: CaptureOptions,
    callback: impl Fn(Event) + Send + Sync + 'static,
  ) -> io::Result<Self> {
    Self::start_source(
      window.native.0.as_ptr(),
      i32::from(options.max_fps()),
      true,
      callback,
    )
  }

  /// Captures a listed window on the main thread with a selected frame-rate cap.
  ///
  /// # Errors
  /// Returns an error if the window is unavailable or capture cannot start.
  pub fn start_window_with_options(
    window: &Window,
    options: CaptureOptions,
    callback: impl Fn(Event) + Send + Sync + 'static,
  ) -> io::Result<Self> {
    Self::start_source(
      window.native.0.as_ptr(),
      i32::from(options.max_fps()),
      false,
      callback,
    )
  }

  /// Returns the selected window ID and owning process on the main thread.
  /// Older system pickers may grant capture without exposing that identity.
  pub fn window_identity(&self) -> Option<(u32, i32)> {
    let (mut id, mut pid) = (0, 0);
    let status = unsafe { ctream_sck_window_identity(self.native.as_ptr(), &mut id, &mut pid) };
    (status != 0).then_some((id, pid))
  }

  fn start_source(
    window: *mut c_void,
    max_fps: i32,
    independent: bool,
    callback: impl Fn(Event) + Send + Sync + 'static,
  ) -> io::Result<Self> {
    let context = Box::into_raw(Box::new(Callback(Box::new(callback))));
    let mut status = 0;
    let native = unsafe {
      ctream_sck_start(
        window,
        max_fps,
        i32::from(independent),
        context.cast(),
        frame,
        end,
        release,
        &mut status,
      )
    };
    if let Some(native) = NonNull::new(native) {
      return Ok(Self {
        native,
        main_thread: PhantomData,
      });
    }
    // On success the bridge owns the context until all callbacks have ended.
    unsafe { drop(Box::from_raw(context)) };
    Err(native_error(status))
  }
}

fn native_error(status: i32) -> io::Error {
  match status {
    1 => io::Error::new(
      io::ErrorKind::Unsupported,
      "window sharing requires macOS 14 or later",
    ),
    2 => io::Error::other("open macOS window capture on the main thread"),
    3 => io::Error::new(
      io::ErrorKind::AlreadyExists,
      "a macOS window capture is already open",
    ),
    4 => io::Error::new(
      io::ErrorKind::PermissionDenied,
      "Screen Recording access is required to select windows from the list",
    ),
    5 => io::Error::new(
      io::ErrorKind::NotFound,
      "the selected window is no longer available",
    ),
    _ => io::Error::other("could not start macOS window capture"),
  }
}

unsafe extern "C" fn listed_window(
  context: *mut c_void,
  handle: *mut c_void,
  id: u32,
  pid: i32,
  title: *const c_char,
  application: *const c_char,
) {
  let Some(native) = NonNull::new(handle).map(NativeWindow) else {
    return;
  };
  // All item callbacks run sequentially before the terminal callback, which
  // drops the context. The bridge holds the strings alive for this call.
  let list = unsafe { &mut *context.cast::<WindowList>() };
  let title = unsafe { CStr::from_ptr(title) }
    .to_string_lossy()
    .into_owned();
  let application = unsafe { CStr::from_ptr(application) }
    .to_string_lossy()
    .into_owned();
  list.windows.push(Window {
    id,
    pid,
    title,
    application,
    native,
  });
}

unsafe extern "C" fn finish_list(context: *mut c_void, message: *const c_char) {
  let WindowList { windows, callback } = *unsafe { Box::from_raw(context.cast::<WindowList>()) };
  let result = if message.is_null() {
    Ok(windows)
  } else {
    Err(io::Error::other(
      unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned(),
    ))
  };
  let _ = catch_unwind(AssertUnwindSafe(|| callback(result)));
}

impl Drop for Capture {
  fn drop(&mut self) {
    unsafe { ctream_sck_stop(self.native.as_ptr()) };
  }
}

fn rgb_from_bgra(size: [usize; 2], stride: usize, data: &[u8]) -> Option<Vec<u8>> {
  let [width, height] = size;
  let row_bytes = width.checked_mul(4)?;
  let count = width.checked_mul(height)?;
  if width == 0 || height == 0 || count > 16 * 1024 * 1024 || stride < row_bytes {
    return None;
  }
  let last_byte = (height - 1).checked_mul(stride)?.checked_add(row_bytes)?;
  let mut rgb = Vec::with_capacity(count * 3);
  for row in data.get(..last_byte)?.chunks(stride) {
    for pixel in row.get(..row_bytes)?.as_chunks::<4>().0 {
      rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
  }
  Some(rgb)
}

unsafe extern "C" fn frame(
  context: *mut c_void,
  bytes: *const u8,
  len: usize,
  width: usize,
  height: usize,
  stride: usize,
) {
  // The bridge holds the pixel buffer lock and owns the context for this call.
  if bytes.is_null() || len > isize::MAX as usize {
    return;
  }
  let bytes = unsafe { slice::from_raw_parts(bytes, len) };
  let size = [width, height];
  if let Some(pixels) = rgb_from_bgra(size, stride, bytes) {
    emit(context, Event::Frame { size, pixels });
  }
}

unsafe extern "C" fn end(context: *mut c_void, kind: i32, message: *const c_char) {
  let event = match kind {
    1 => Event::Cancelled,
    2 => Event::Finished,
    _ => {
      let message = if message.is_null() {
        "ScreenCaptureKit could not capture the window".into()
      } else {
        unsafe { CStr::from_ptr(message) }
          .to_string_lossy()
          .into_owned()
      };
      Event::Failed(message)
    }
  };
  emit(context, event);
}

fn emit(context: *mut c_void, event: Event) {
  // Never unwind through ScreenCaptureKit's Objective-C callbacks.
  let callback = unsafe { &*context.cast::<Callback>() };
  let _ = catch_unwind(AssertUnwindSafe(|| (callback.0)(event)));
}

unsafe extern "C" fn release(context: *mut c_void) {
  let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
    drop(Box::from_raw(context.cast::<Callback>()))
  }));
}

unsafe extern "C" {
  fn ctream_sck_is_supported() -> i32;
  fn ctream_sck_has_screen_recording_access() -> i32;
  fn ctream_sck_list_windows(
    context: *mut c_void,
    window: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, i32, *const c_char, *const c_char),
    completion: unsafe extern "C" fn(*mut c_void, *const c_char),
  ) -> i32;
  fn ctream_sck_release_window(window: *mut c_void);
  fn ctream_sck_start(
    window: *mut c_void,
    max_fps: i32,
    independent: i32,
    context: *mut c_void,
    frame: unsafe extern "C" fn(*mut c_void, *const u8, usize, usize, usize, usize),
    end: unsafe extern "C" fn(*mut c_void, i32, *const c_char),
    release: unsafe extern "C" fn(*mut c_void),
    status: *mut i32,
  ) -> *mut c_void;
  fn ctream_sck_window_identity(native: *mut c_void, id: *mut u32, owner: *mut i32) -> i32;
  fn ctream_sck_stop(native: *mut c_void);
}

#[cfg(test)]
mod tests {
  use super::rgb_from_bgra;

  #[test]
  fn converts_bgra_using_row_stride_and_rejects_truncation() {
    let bgra = [1, 2, 3, 255, 0, 0, 0, 0, 4, 5, 6, 255];
    assert_eq!(
      rgb_from_bgra([1, 2], 8, &bgra),
      Some(vec![3, 2, 1, 6, 5, 4])
    );
    assert_eq!(rgb_from_bgra([1, 2], 8, &bgra[..11]), None);
    assert_eq!(rgb_from_bgra([2, 1], 7, &bgra), None);
  }
}
