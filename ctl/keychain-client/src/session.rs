//! Revoke credential authorization when the console locks or the Mac sleeps.
//!
//! Distributed notifications require the main `CFRunLoop`. The daemon performs its
//! async work on a scoped worker while this module services that run loop. No
//! credential material crosses this boundary.

use core_foundation::base::{CFAllocatorRef, CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::runloop::{
  CFRunLoop, CFRunLoopSource, CFRunLoopSourceRef, kCFRunLoopDefaultMode,
};
use core_foundation::string::{CFString, CFStringRef};
use std::collections::BTreeMap;
use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::Duration;

type Revoke = Arc<dyn Fn() + Send + Sync>;
type NotificationCenter = *mut c_void;
type NotificationPort = *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
  fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
  fn CFNotificationCenterGetDistributedCenter() -> NotificationCenter;
  fn CFNotificationCenterAddObserver(
    center: NotificationCenter,
    observer: *const c_void,
    callback: extern "C" fn(
      NotificationCenter,
      *mut c_void,
      CFStringRef,
      *const c_void,
      CFDictionaryRef,
    ),
    name: CFStringRef,
    object: *const c_void,
    suspension_behavior: isize,
  );
  fn CFNotificationCenterRemoveEveryObserver(center: NotificationCenter, observer: *const c_void);
  fn CFNotificationCenterPostNotification(
    center: NotificationCenter,
    name: CFStringRef,
    object: *const c_void,
    user_info: CFDictionaryRef,
    deliver_immediately: u8,
  );
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
  fn IORegisterForSystemPower(
    context: *mut c_void,
    port: *mut NotificationPort,
    callback: extern "C" fn(*mut c_void, u32, u32, *mut c_void),
    notifier: *mut u32,
  ) -> u32;
  fn IONotificationPortGetRunLoopSource(port: NotificationPort) -> CFRunLoopSourceRef;
  fn IODeregisterForSystemPower(notifier: *mut u32) -> i32;
  fn IONotificationPortDestroy(port: NotificationPort);
  fn IOServiceClose(connection: u32) -> i32;
  fn IOAllowPowerChange(connection: u32, notification_id: isize) -> i32;
  fn IORegistryGetRootEntry(main_port: u32) -> u32;
  fn IORegistryEntryCreateCFProperty(
    entry: u32,
    key: CFStringRef,
    allocator: CFAllocatorRef,
    options: u32,
  ) -> CFTypeRef;
  fn IOObjectRelease(object: u32) -> i32;
  fn IORegistryEntryGetChildEntry(entry: u32, plane: *const c_char, child: *mut u32) -> i32;
  fn IOServiceAddInterestNotification(
    port: NotificationPort,
    service: u32,
    interest: *const c_char,
    callback: extern "C" fn(*mut c_void, u32, u32, *mut c_void),
    context: *mut c_void,
    notifier: *mut u32,
  ) -> i32;
}

const CAN_SLEEP: u32 = 0xe000_0270;
const WILL_SLEEP: u32 = 0xe000_0280;
const HAS_POWERED_ON: u32 = 0xe000_0300;
const CONSOLE_SECURITY_CHANGE: u32 = 0xe000_0128;
const DELIVER_IMMEDIATELY: isize = 4;

/// A non-secret console-state signal owned by the daemon's main run loop.
pub struct SessionState {
  available: AtomicBool,
  unlocked: AtomicBool,
  generation: AtomicU64,
  next_subscription: AtomicU64,
  subscribers: Mutex<BTreeMap<u64, Revoke>>,
}

impl SessionState {
  fn new() -> Self {
    Self {
      available: AtomicBool::new(false),
      unlocked: AtomicBool::new(false),
      generation: AtomicU64::new(0),
      next_subscription: AtomicU64::new(0),
      subscribers: Mutex::new(BTreeMap::new()),
    }
  }

  /// Return false when registration or the current console state is unknown.
  /// Call this before and after using an authorization context, along with its
  /// generation. Reading state does not invoke subscribers reentrantly.
  #[must_use]
  pub fn is_unlocked(&self) -> bool {
    self.available.load(Ordering::Acquire)
      && self.unlocked.load(Ordering::Acquire)
      && console_is_unlocked()
  }

  /// Every delivered lock or sleep event changes the approval epoch, even if
  /// the console has already unlocked by the time its notification arrives.
  #[must_use]
  pub fn generation(&self) -> u64 {
    self.generation.load(Ordering::Acquire)
  }

  /// Subscribe a fast revocation callback. It must not block on a connection,
  /// perform Keychain I/O, or panic: sleep waits for these callbacks to finish.
  pub fn subscribe(self: &Arc<Self>, revoke: Revoke) -> SessionSubscription {
    let id = self.next_subscription.fetch_add(1, Ordering::Relaxed);
    let mut subscribers = self
      .subscribers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    subscribers.insert(id, revoke);
    SessionSubscription {
      state: Arc::downgrade(self),
      id,
    }
  }

  fn revoke(&self) {
    self.unlocked.store(false, Ordering::Release);
    self.generation.fetch_add(1, Ordering::AcqRel);
    let callbacks: Vec<_> = self
      .subscribers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .values()
      .cloned()
      .collect();
    for revoke in callbacks {
      if catch_unwind(AssertUnwindSafe(|| revoke())).is_err() {
        self.available.store(false, Ordering::Release);
      }
    }
  }

  fn console_changed(&self, unlocked: bool) {
    // Secure-input changes also publish console security metadata. They must
    // not cancel the account-password dialog that authorizes a Keychain read.
    // The explicit distributed lock event remains unconditional; the kernel
    // notification is an additional early signal while the console is locked.
    if !unlocked {
      self.revoke();
    }
  }
}

/// Retain this guard for as long as authorization contexts can be reused.
pub struct SessionSubscription {
  state: Weak<SessionState>,
  id: u64,
}

impl Drop for SessionSubscription {
  fn drop(&mut self) {
    if let Some(state) = self.state.upgrade() {
      state
        .subscribers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&self.id);
    }
  }
}

/// Run daemon work while macOS's main run loop receives lock/sleep events.
/// Unknown console state disables authorization reuse, without preventing work.
///
/// # Panics
/// Propagates a panic from the daemon worker.
pub fn with_session_monitor<R: Send>(work: impl FnOnce(Arc<SessionState>) -> R + Send) -> R {
  let state = Arc::new(SessionState::new());
  let current = CFRunLoop::get_current();
  let main = CFRunLoop::get_main();
  if current.as_concrete_TypeRef() != main.as_concrete_TypeRef() {
    return work(state);
  }
  let Some(_monitor) = Monitor::start(Arc::clone(&state), current) else {
    return work(state);
  };
  std::thread::scope(|scope| {
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = scope.spawn(move || {
      let result = work(state);
      let _ = sender.send(result);
    });
    loop {
      match receiver.try_recv() {
        Ok(result) => {
          worker.join().expect("credential session worker panicked");
          return result;
        }
        Err(mpsc::TryRecvError::Disconnected) => {
          worker.join().expect("credential session worker panicked");
          unreachable!("credential session worker did not return its result");
        }
        Err(mpsc::TryRecvError::Empty) => {}
      }
      // A bounded run keeps worker completion responsive; event delivery itself
      // is driven by the notification sources, never by polling the lock state.
      let _ = CFRunLoop::run_in_mode(
        unsafe { kCFRunLoopDefaultMode },
        Duration::from_millis(50),
        false,
      );
    }
  })
}

fn boolean(dictionary: &CFDictionary<CFString, CFType>, key: &str) -> Option<bool> {
  dictionary
    .find(CFString::new(key))?
    .downcast::<CFBoolean>()
    .map(Into::into)
}

fn console_lock_state() -> Option<bool> {
  // Apple's IOService::updateConsoleUsers publishes this explicit Boolean on
  // the registry root. False explicitly proves that the console is unlocked:
  // https://github.com/apple-oss-distributions/xnu/blob/main/iokit/Kernel/IOService.cpp
  // SAFETY: public IOKit APIs return owned handles/references. Every path
  // releases the registry handle and transfers the property to an owned CFType.
  let root = unsafe { IORegistryGetRootEntry(0) };
  if root == 0 {
    return None;
  }
  let key = CFString::new("IOConsoleLocked");
  let property =
    unsafe { IORegistryEntryCreateCFProperty(root, key.as_concrete_TypeRef(), ptr::null(), 0) };
  unsafe {
    IOObjectRelease(root);
  }
  if property.is_null() {
    return None;
  }
  let value: CFType = unsafe { TCFType::wrap_under_create_rule(property) };
  value.downcast::<CFBoolean>().map(Into::into)
}

fn console_is_unlocked() -> bool {
  // SAFETY: the public Copy API returns an owned dictionary or null. It contains
  // only console metadata, never credentials. Strict booleans fail closed.
  let raw = unsafe { CGSessionCopyCurrentDictionary() };
  if raw.is_null() {
    return false;
  }
  let dictionary: CFDictionary<CFString, CFType> = unsafe { TCFType::wrap_under_create_rule(raw) };
  console_lock_state() == Some(false)
    && boolean(&dictionary, "kCGSSessionOnConsoleKey") == Some(true)
    && boolean(&dictionary, "kCGSessionLoginDoneKey") == Some(true)
}

struct CallbackState {
  state: Arc<SessionState>,
  connection: u32,
  probe: String,
}

struct Monitor {
  callback: Box<CallbackState>,
  center: NotificationCenter,
  port: NotificationPort,
  notifier: u32,
  console_notifier: u32,
  run_loop: CFRunLoop,
  source: Option<CFRunLoopSource>,
}

impl Monitor {
  fn start(state: Arc<SessionState>, run_loop: CFRunLoop) -> Option<Self> {
    // SAFETY: this process-lifetime center is only used from its main run loop.
    let center = unsafe { CFNotificationCenterGetDistributedCenter() };
    if center.is_null() {
      return None;
    }
    let mut monitor = Self {
      callback: Box::new(CallbackState {
        state,
        connection: 0,
        probe: format!("dev.tokn-ai.ctl.session-monitor.{}", std::process::id()),
      }),
      center,
      port: ptr::null_mut(),
      notifier: 0,
      console_notifier: 0,
      run_loop,
      source: None,
    };
    let observer = (&raw mut *monitor.callback).cast::<c_void>();
    // SAFETY: the boxed callback is stable until observers are removed on this
    // same thread. IOKit owns the port and notifier returned through live slots.
    monitor.callback.connection = unsafe {
      IORegisterForSystemPower(
        observer,
        &raw mut monitor.port,
        power_event,
        &raw mut monitor.notifier,
      )
    };
    if monitor.callback.connection == 0 || monitor.port.is_null() {
      return None;
    }
    if !monitor.observe_console_security(observer) {
      return None;
    }
    let raw_source = unsafe { IONotificationPortGetRunLoopSource(monitor.port) };
    if raw_source.is_null() {
      return None;
    }
    let source = unsafe { CFRunLoopSource::wrap_under_get_rule(raw_source) };
    monitor
      .run_loop
      .add_source(&source, unsafe { kCFRunLoopDefaultMode });
    monitor.source = Some(source);
    // Lock/unlock notification names are WindowServer conventions, rather than
    // public SDK constants. A delivery self-probe and strict current-state
    // verification make missing session/notification infrastructure fail closed.
    for name in [
      "com.apple.screenIsLocked",
      "com.apple.screenIsUnlocked",
      "com.apple.sessionDidMoveOffConsole",
      "com.apple.sessionDidMoveOnConsole",
      "com.apple.userWillLogOut",
      monitor.callback.probe.as_str(),
    ] {
      let name = CFString::new(name);
      unsafe {
        CFNotificationCenterAddObserver(
          center,
          observer,
          distributed_event,
          name.as_concrete_TypeRef(),
          ptr::null(),
          DELIVER_IMMEDIATELY,
        );
      }
    }
    let probe = CFString::new(&monitor.callback.probe);
    unsafe {
      CFNotificationCenterPostNotification(
        center,
        probe.as_concrete_TypeRef(),
        ptr::null(),
        ptr::null(),
        1,
      );
    }
    Some(monitor)
  }

  fn observe_console_security(&mut self, observer: *mut c_void) -> bool {
    // Apple's XNU sends ConsoleSecurityChange to IOConsoleSecurityInterest on
    // the service-plane root whenever console users/lock state is published:
    // https://github.com/apple-oss-distributions/xnu/blob/main/iokit/Kernel/IOService.cpp
    // Missing service or unsupported interest fails closed.
    // SAFETY: root/child handles are owned, released after registration, and the
    // returned notifier retains its service until teardown before the port.
    let root = unsafe { IORegistryGetRootEntry(0) };
    if root == 0 {
      return false;
    }
    let mut service = 0;
    let status =
      unsafe { IORegistryEntryGetChildEntry(root, c"IOService".as_ptr(), &raw mut service) };
    unsafe {
      IOObjectRelease(root);
    }
    if status != 0 || service == 0 {
      return false;
    }
    let status = unsafe {
      IOServiceAddInterestNotification(
        self.port,
        service,
        c"IOConsoleSecurityInterest".as_ptr(),
        console_event,
        observer,
        &raw mut self.console_notifier,
      )
    };
    unsafe {
      IOObjectRelease(service);
    }
    status == 0 && self.console_notifier != 0
  }
}

impl Drop for Monitor {
  fn drop(&mut self) {
    self
      .callback
      .state
      .available
      .store(false, Ordering::Release);
    self.callback.state.revoke();
    let observer = (&raw const *self.callback).cast::<c_void>();
    // SAFETY: teardown runs on the registering main thread, before the boxed
    // callback is freed. The IOKit SDK specifies this deregistration order.
    unsafe {
      CFNotificationCenterRemoveEveryObserver(self.center, observer);
      if let Some(source) = self.source.take() {
        self.run_loop.remove_source(&source, kCFRunLoopDefaultMode);
      }
      if self.notifier != 0 {
        IODeregisterForSystemPower(&raw mut self.notifier);
      }
      if self.console_notifier != 0 {
        IOObjectRelease(self.console_notifier);
      }
      if !self.port.is_null() {
        IONotificationPortDestroy(self.port);
      }
      if self.callback.connection != 0 {
        IOServiceClose(self.callback.connection);
      }
    }
  }
}

extern "C" fn distributed_event(
  _center: NotificationCenter,
  observer: *mut c_void,
  name: CFStringRef,
  _object: *const c_void,
  _info: CFDictionaryRef,
) {
  // SAFETY: registration owns this box until callbacks have been unregistered
  // on the same main thread. The name is a borrowed CFString for this callback.
  let callback = unsafe { &*observer.cast::<CallbackState>() };
  let name = unsafe { CFString::wrap_under_get_rule(name) }.to_string();
  if name == callback.probe {
    callback
      .state
      .unlocked
      .store(console_is_unlocked(), Ordering::Release);
    callback.state.available.store(true, Ordering::Release);
  } else if name == "com.apple.screenIsUnlocked" || name == "com.apple.sessionDidMoveOnConsole" {
    callback
      .state
      .unlocked
      .store(console_is_unlocked(), Ordering::Release);
  } else {
    callback.state.revoke();
  }
}

extern "C" fn console_event(
  context: *mut c_void,
  _service: u32,
  message: u32,
  _argument: *mut c_void,
) {
  // SAFETY: the interest notifier delivers callbacks on our main run loop while
  // this stable box lives. Console metadata also changes for secure input;
  // only a currently locked/inactive console revokes through this channel.
  let callback = unsafe { &*context.cast::<CallbackState>() };
  if message == CONSOLE_SECURITY_CHANGE {
    callback.state.console_changed(console_is_unlocked());
  }
}

extern "C" fn power_event(
  context: *mut c_void,
  _service: u32,
  message: u32,
  argument: *mut c_void,
) {
  // SAFETY: IOKit delivers callbacks on our main run loop while this box lives.
  let callback = unsafe { &*context.cast::<CallbackState>() };
  if message == WILL_SLEEP {
    callback.state.revoke();
  }
  if message == HAS_POWERED_ON {
    callback
      .state
      .unlocked
      .store(console_is_unlocked(), Ordering::Release);
  }
  if message == CAN_SLEEP || message == WILL_SLEEP {
    // Never prevent or postpone system sleep; revocation completes before ack.
    unsafe {
      IOAllowPowerChange(callback.connection, argument as isize);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unmonitored_state_fails_closed() {
    let state = SessionState::new();
    assert!(!state.is_unlocked());
  }

  #[test]
  fn revocation_updates_generation_and_removes_subscriptions() {
    let state = Arc::new(SessionState::new());
    let revoked = Arc::new(AtomicU64::new(0));
    let count = Arc::clone(&revoked);
    let guard = state.subscribe(Arc::new(move || {
      count.fetch_add(1, Ordering::Relaxed);
    }));
    state.revoke();
    assert_eq!(state.generation(), 1);
    assert_eq!(revoked.load(Ordering::Relaxed), 1);
    drop(guard);
    state.revoke();
    assert_eq!(revoked.load(Ordering::Relaxed), 1);
  }

  #[test]
  fn panicking_subscription_disables_monitor_and_preserves_other_revocations() {
    let state = Arc::new(SessionState::new());
    state.available.store(true, Ordering::Release);
    let revoked = Arc::new(AtomicBool::new(false));
    let count = Arc::clone(&revoked);
    let _first = state.subscribe(Arc::new(|| panic!("subscriber failed")));
    let _second = state.subscribe(Arc::new(move || count.store(true, Ordering::Relaxed)));
    state.revoke();
    assert!(!state.available.load(Ordering::Acquire));
    assert!(revoked.load(Ordering::Relaxed));
  }

  #[test]
  fn secure_input_metadata_changes_do_not_revoke_approval() {
    let state = Arc::new(SessionState::new());
    state.unlocked.store(true, Ordering::Release);
    let revoked = Arc::new(AtomicU64::new(0));
    let count = Arc::clone(&revoked);
    let _guard = state.subscribe(Arc::new(move || {
      count.fetch_add(1, Ordering::Relaxed);
    }));
    state.console_changed(true);
    assert_eq!(state.generation(), 0);
    assert_eq!(revoked.load(Ordering::Relaxed), 0);
    state.console_changed(false);
    assert_eq!(state.generation(), 1);
    assert_eq!(revoked.load(Ordering::Relaxed), 1);
  }

  #[test]
  fn delayed_lock_event_revokes_after_console_has_unlocked() {
    let state = Arc::new(SessionState::new());
    state.unlocked.store(true, Ordering::Release);
    let revoked = Arc::new(AtomicU64::new(0));
    let count = Arc::clone(&revoked);
    let _guard = state.subscribe(Arc::new(move || {
      count.fetch_add(1, Ordering::Relaxed);
    }));
    let mut callback = CallbackState {
      state: Arc::clone(&state),
      connection: 0,
      probe: String::new(),
    };
    let name = CFString::new("com.apple.screenIsLocked");
    distributed_event(
      ptr::null_mut(),
      (&raw mut callback).cast::<c_void>(),
      name.as_concrete_TypeRef(),
      ptr::null(),
      ptr::null(),
    );
    assert_eq!(state.generation(), 1);
    assert_eq!(revoked.load(Ordering::Relaxed), 1);
    assert!(!state.unlocked.load(Ordering::Acquire));
  }
}
