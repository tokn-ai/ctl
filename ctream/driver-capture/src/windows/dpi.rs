use windows::{
  Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
  },
  core::{Error, Result},
};

/// Providers use physical pixels without changing the embedding process's policy.
pub(super) struct PhysicalPixels(DPI_AWARENESS_CONTEXT);

impl PhysicalPixels {
  pub(super) fn enter() -> Result<Self> {
    let previous =
      unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if previous.0.is_null() {
      Err(Error::from_thread())
    } else {
      Ok(Self(previous))
    }
  }
}

impl Drop for PhysicalPixels {
  fn drop(&mut self) {
    // The raw context handle makes the guard !Send, so restoration stays on its thread.
    unsafe { SetThreadDpiAwarenessContext(self.0) };
  }
}
