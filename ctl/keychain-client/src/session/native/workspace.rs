//! Public display notifications distinguish full wake from background `DarkWake`.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};

use super::{SessionState, console_is_unlocked};

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
  static NSWorkspaceScreensDidWakeNotification: *const AnyObject;
  static NSWorkspaceScreensDidSleepNotification: *const AnyObject;
}

pub(super) struct Monitor {
  center: Retained<AnyObject>,
  observer: Retained<DisplayObserver>,
}

impl Monitor {
  pub(super) fn start(state: Arc<SessionState>) -> Option<Self> {
    let workspace_class = AnyClass::get(c"NSWorkspace")?;
    // SAFETY: AppKit supplies these public NSObject APIs. Retained wrappers
    // retain autoreleased results; a failed allocation disables resume only.
    let workspace: Option<Retained<AnyObject>> =
      unsafe { msg_send![workspace_class, sharedWorkspace] };
    let center: Option<Retained<AnyObject>> =
      unsafe { msg_send![&*workspace?, notificationCenter] };
    let center = center?;
    let observer = DisplayObserver::new(state)?;
    // SAFETY: public NSNotificationCenter observer APIs accept this object's
    // registered NSObject notification methods. The observer owns its state.
    unsafe {
      let _: () = msg_send![&*center,
        addObserver: &*observer,
        selector: sel!(screensWoke:),
        name: NSWorkspaceScreensDidWakeNotification,
        object: std::ptr::null::<AnyObject>()
      ];
      let _: () = msg_send![&*center,
        addObserver: &*observer,
        selector: sel!(screensSlept:),
        name: NSWorkspaceScreensDidSleepNotification,
        object: std::ptr::null::<AnyObject>()
      ];
    }
    Some(Self { center, observer })
  }
}

impl Drop for Monitor {
  fn drop(&mut self) {
    // SAFETY: teardown happens on the registering main run loop before releasing
    // our retained observer. Its owned Arc remains valid during any delivery.
    unsafe {
      let _: () = msg_send![&*self.center, removeObserver: &*self.observer];
    }
  }
}

define_class!(
  // SAFETY: NSObject has no subclassing requirements. objc2 owns and drops the
  // Arc ivar with the Objective-C object, including any retained notifications.
  #[unsafe(super = NSObject)]
  #[name = "CtlCredentialSessionDisplayObserver"]
  #[ivars = Arc<SessionState>]
  struct DisplayObserver;

  impl DisplayObserver {
    // SAFETY: this is the registered NSObject notification callback signature.
    #[unsafe(method(screensWoke:))]
    fn screens_woke(&self, _notification: &AnyObject) {
      self.ivars().screens_changed(true, console_is_unlocked());
    }

    // SAFETY: this is the registered NSObject notification callback signature.
    #[unsafe(method(screensSlept:))]
    fn screens_slept(&self, _notification: &AnyObject) {
      self.ivars().screens_changed(false, console_is_unlocked());
    }
  }
);

impl DisplayObserver {
  fn new(state: Arc<SessionState>) -> Option<Retained<Self>> {
    let allocated = Self::alloc().set_ivars(state);
    // SAFETY: NSObject.init has this signature and no additional invariants.
    unsafe { msg_send![super(allocated), init] }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn observer_owns_its_state_and_registered_selectors_deliver_display_events() {
    let state = Arc::new(SessionState::new(console_is_unlocked));
    let weak = Arc::downgrade(&state);
    let observer = DisplayObserver::new(Arc::clone(&state)).unwrap();
    let notification = NSObject::new();
    // SAFETY: these are the registered notification signatures. An unmonitored
    // state cannot emit resume or authorize anything; only display bits change.
    unsafe {
      let _: () = msg_send![&*observer, screensWoke: &*notification];
      assert!(state.wake.lock().unwrap().screens_awake);
      let _: () = msg_send![&*observer, screensSlept: &*notification];
      assert!(!state.wake.lock().unwrap().screens_awake);
    }
    drop(state);
    assert!(weak.upgrade().is_some());
    drop(observer);
    assert!(weak.upgrade().is_none());
  }
}
