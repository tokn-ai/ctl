#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <Foundation/Foundation.h>
#include <libproc.h>
#include <math.h>
#include <unistd.h>

typedef struct {
  uint32_t id;
  int32_t pid;
  uint64_t started_seconds;
  uint64_t started_microseconds;
} CtreamInputTarget;

enum CtreamPointerEvent {
  CtreamMove,
  CtreamDown,
  CtreamDrag,
  CtreamUp,
  CtreamScroll,
};

bool ctream_macos_input_has_access(void) {
  if (@available(macOS 10.15, *)) {
    return CGPreflightPostEventAccess() && AXIsProcessTrusted();
  }
  return false;
}

static bool process_info(int32_t pid, struct proc_bsdinfo *info) {
  return pid > 0 &&
      proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, info, sizeof(*info)) == sizeof(*info);
}

static bool same_process(const CtreamInputTarget *target) {
  struct proc_bsdinfo info;
  return process_info(target->pid, &info) &&
      info.pbi_start_tvsec == target->started_seconds &&
      info.pbi_start_tvusec == target->started_microseconds;
}

static bool window_bounds(NSDictionary *entry, CGRect *bounds) {
  NSDictionary *values = entry[(__bridge NSString *)kCGWindowBounds];
  return values && CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)values, bounds) &&
      isfinite(bounds->origin.x) && isfinite(bounds->origin.y) &&
      isfinite(bounds->size.width) && isfinite(bounds->size.height) &&
      bounds->size.width > 0 && bounds->size.height > 0;
}

static bool owned_window(NSDictionary *entry, const CtreamInputTarget *target) {
  return [entry[(__bridge NSString *)kCGWindowNumber] unsignedIntValue] == target->id &&
      [entry[(__bridge NSString *)kCGWindowOwnerPID] intValue] == target->pid;
}

bool ctream_macos_input_bind(CtreamInputTarget *target) {
  @autoreleasepool {
    if (target->id == kCGNullWindowID || target->pid == getpid()) {
      return false;
    }
    struct proc_bsdinfo info;
    if (!process_info(target->pid, &info)) {
      return false;
    }
    NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(
        kCGWindowListOptionIncludingWindow, target->id));
    CGRect bounds;
    if (windows.count != 1 || !owned_window(windows[0], target) ||
        !window_bounds(windows[0], &bounds)) {
      return false;
    }
    target->started_seconds = info.pbi_start_tvsec;
    target->started_microseconds = info.pbi_start_tvusec;
    return same_process(target);
  }
}

static bool read_ax_value(AXUIElementRef window, CFStringRef attribute,
                          AXValueType type, void *result) {
  CFTypeRef value = NULL;
  if (AXUIElementCopyAttributeValue(window, attribute, &value) != kAXErrorSuccess || !value) {
    return false;
  }
  bool valid = CFGetTypeID(value) == AXValueGetTypeID() &&
      AXValueGetValue((AXValueRef)value, type, result);
  CFRelease(value);
  return valid;
}

static bool focused_bounds(int32_t pid, CGRect *bounds) {
  AXUIElementRef app = AXUIElementCreateApplication(pid);
  if (!app) {
    return false;
  }
  AXUIElementSetMessagingTimeout(app, 0.1);
  CFTypeRef focused = NULL;
  AXError result = AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute, &focused);
  CFRelease(app);
  if (result != kAXErrorSuccess || !focused) {
    return false;
  }
  bool valid = false;
  if (CFGetTypeID(focused) == AXUIElementGetTypeID()) {
    AXUIElementRef window = (AXUIElementRef)focused;
    AXUIElementSetMessagingTimeout(window, 0.1);
    valid = read_ax_value(window, kAXPositionAttribute, kAXValueCGPointType, &bounds->origin) &&
        read_ax_value(window, kAXSizeAttribute, kAXValueCGSizeType, &bounds->size);
  }
  CFRelease(focused);
  return valid;
}

static bool matches_bounds(CGRect left, CGRect right) {
  return fabs(left.origin.x - right.origin.x) <= 1 &&
      fabs(left.origin.y - right.origin.y) <= 1 &&
      fabs(left.size.width - right.size.width) <= 1 &&
      fabs(left.size.height - right.size.height) <= 1;
}

// Quartz posts to a process. Corroborate its Accessibility focused window with
// the selected Quartz window's unique geometry before sending any new event.
// Mouse input also requires the point to be clear of windows above the target
// from the same process. Windows owned by other processes are never recipients.
static bool route(const CtreamInputTarget *target, const double *fraction, CGRect *window) {
  CGRect focused;
  if (!same_process(target) || !focused_bounds(target->pid, &focused)) {
    return false;
  }
  NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(
      kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID));
  NSUInteger selected = NSNotFound;
  NSUInteger index = 0;
  for (NSDictionary *entry in windows) {
    NSUInteger current = index++;
    if ([entry[(__bridge NSString *)kCGWindowOwnerPID] intValue] != target->pid) {
      continue;
    }
    CGRect bounds;
    if (!window_bounds(entry, &bounds)) {
      continue;
    }
    if (owned_window(entry, target)) {
      if (!matches_bounds(focused, bounds)) {
        return false;
      }
      selected = current;
      if (window) {
        *window = bounds;
      }
    } else if (matches_bounds(focused, bounds)) {
      return false;
    }
  }
  if (selected == NSNotFound) {
    return false;
  }
  if (fraction) {
    CGPoint point = CGPointMake(window->origin.x + fraction[0] * window->size.width,
                                window->origin.y + fraction[1] * window->size.height);
    for (NSUInteger i = 0; i < selected; ++i) {
      NSDictionary *entry = windows[i];
      CGRect bounds;
      if ([entry[(__bridge NSString *)kCGWindowOwnerPID] intValue] == target->pid &&
          window_bounds(entry, &bounds) && CGRectContainsPoint(bounds, point)) {
        return false;
      }
    }
  }
  return true;
}

static int post(CGEventRef event, const CtreamInputTarget *target, uint64_t flags) {
  if (!event) {
    return -1;
  }
  bool deliver = same_process(target);
  if (deliver) {
    CGEventSetFlags(event, flags);
    CGEventPostToPid(target->pid, event);
  }
  CFRelease(event);
  return deliver ? 1 : 0;
}

int ctream_macos_input_key(const CtreamInputTarget *target, uint16_t key, bool pressed,
                           bool modifier, uint64_t flags) {
  @autoreleasepool {
    if (pressed && !route(target, NULL, NULL)) {
      return 0;
    }
    CGEventSourceRef source = CGEventSourceCreate(kCGEventSourceStatePrivate);
    if (!source) {
      return -1;
    }
    CGEventRef event = CGEventCreateKeyboardEvent(source, key, pressed);
    CFRelease(source);
    if (event && modifier) {
      CGEventSetType(event, kCGEventFlagsChanged);
    }
    return post(event, target, flags);
  }
}

static NSEventType mouse_type(uint8_t action, uint8_t button) {
  if (action == CtreamMove) {
    return NSEventTypeMouseMoved;
  }
  if (button == kCGMouseButtonLeft) {
    if (action == CtreamDown) return NSEventTypeLeftMouseDown;
    if (action == CtreamUp) return NSEventTypeLeftMouseUp;
    return NSEventTypeLeftMouseDragged;
  }
  if (button == kCGMouseButtonRight) {
    if (action == CtreamDown) return NSEventTypeRightMouseDown;
    if (action == CtreamUp) return NSEventTypeRightMouseUp;
    return NSEventTypeRightMouseDragged;
  }
  if (action == CtreamDown) return NSEventTypeOtherMouseDown;
  if (action == CtreamUp) return NSEventTypeOtherMouseUp;
  return NSEventTypeOtherMouseDragged;
}

static CGEventRef mouse_event(const CtreamInputTarget *target, uint8_t action, uint8_t button,
                              CGPoint point, uint64_t flags, uint32_t click_count) {
  // AppKit's factory encodes the window number; the Quartz window-under-pointer
  // fields alone do not. The foreign window is absent from this process's
  // AppKit window list, so AppKit flips its input Y against the primary screen.
  // Compensate for that conversion to preserve the top-left window-local point.
  CGFloat primary_height = CGDisplayBounds(CGMainDisplayID()).size.height;
  NSEvent *mouse = [NSEvent mouseEventWithType:mouse_type(action, button)
      location:NSMakePoint(point.x, primary_height - point.y)
      modifierFlags:flags timestamp:NSProcessInfo.processInfo.systemUptime
      windowNumber:target->id context:nil eventNumber:0 clickCount:click_count
      pressure:action == CtreamDown || action == CtreamDrag ? 1.0 : 0.0];
  CGEventRef event = mouse.CGEvent ? CGEventCreateCopy(mouse.CGEvent) : NULL;
  if (event) {
    CGEventSetIntegerValueField(event, kCGMouseEventButtonNumber, button);
  }
  return event;
}

static CGEventRef scroll_event(const CtreamInputTarget *target, uint8_t button, CGPoint point,
                               uint64_t flags) {
  int32_t vertical = (button == 4) - (button == 5);
  int32_t horizontal = (button == 6) - (button == 7);
  CGEventRef deltas = CGEventCreateScrollWheelEvent(NULL, kCGScrollEventUnitLine, 2, vertical,
                                                    horizontal);
  if (!deltas) {
    return NULL;
  }
  // AppKit has no scroll factory that accepts a window number. Preserve the
  // window association from a mouse move and copy the public Quartz deltas.
  CGEventRef event = mouse_event(target, CtreamMove, kCGMouseButtonLeft, point, flags, 0);
  if (event) {
    CGEventSetType(event, kCGEventScrollWheel);
    const CGEventField fields[] = {
      kCGScrollWheelEventDeltaAxis1, kCGScrollWheelEventDeltaAxis2,
      kCGScrollWheelEventFixedPtDeltaAxis1, kCGScrollWheelEventFixedPtDeltaAxis2,
      kCGScrollWheelEventPointDeltaAxis1, kCGScrollWheelEventPointDeltaAxis2,
      kCGScrollWheelEventIsContinuous,
    };
    for (size_t i = 0; i < sizeof(fields) / sizeof(fields[0]); ++i) {
      CGEventSetDoubleValueField(event, fields[i], CGEventGetDoubleValueField(deltas, fields[i]));
    }
  }
  CFRelease(deltas);
  return event;
}

int ctream_macos_input_pointer(const CtreamInputTarget *target, uint8_t action, uint8_t button,
                               const double *fraction, double *position, uint64_t flags,
                               uint32_t click_count) {
  @autoreleasepool {
    CGRect window;
    CGPoint point = CGPointMake(position[0], position[1]);
    if (action != CtreamUp) {
      if (!route(target, fraction, &window)) {
        return 0;
      }
      point = CGPointMake(fraction[0] * window.size.width, fraction[1] * window.size.height);
    }
    CGEventRef event;
    if (action == CtreamScroll) {
      event = scroll_event(target, button, point, flags);
    } else {
      event = mouse_event(target, action, button, point, flags, click_count);
    }
    if (event) {
      CGEventSetIntegerValueField(event, kCGMouseEventWindowUnderMousePointer, target->id);
      CGEventSetIntegerValueField(event, kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent,
                                 target->id);
    }
    position[0] = point.x;
    position[1] = point.y;
    return post(event, target, flags);
  }
}
