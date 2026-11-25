#import <CoreGraphics/CoreGraphics.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <dispatch/dispatch.h>
#import <dlfcn.h>
#import <math.h>
#import <stdatomic.h>
#import <stdint.h>

typedef void (*FrameCallback)(void *, const uint8_t *, size_t, size_t, size_t, size_t);
typedef void (*EndCallback)(void *, int32_t, const char *);
typedef void (*ReleaseCallback)(void *);
typedef void (*WindowCallback)(void *, void *, uint32_t, int32_t, const char *, const char *);
typedef void (*WindowListCallback)(void *, const char *);

typedef NS_ENUM(int32_t, CaptureStatus) {
  CaptureStatusReady,
  CaptureStatusUnsupported,
  CaptureStatusWrongThread,
  CaptureStatusOccupied,
  CaptureStatusUnauthorized,
  CaptureStatusWindowUnavailable,
};

typedef NS_ENUM(int32_t, CaptureEnd) {
  CaptureEndSilent,
  CaptureEndCancelled,
  CaptureEndFinished,
  CaptureEndFailed,
};

typedef NS_ENUM(int, SelectedWindowState) {
  SelectedWindowUnknown,
  SelectedWindowMissing,
  SelectedWindowPresent,
};

static SelectedWindowState selectedWindowState(CGWindowID id, pid_t owner) {
  // Query the selected ID even when the window is offscreen or minimized.
  NSArray *windows = CFBridgingRelease(CGWindowListCopyWindowInfo(kCGWindowListOptionIncludingWindow, id));
  if (!windows) {
    return SelectedWindowUnknown;
  }
  NSDictionary *window = windows.firstObject;
  if ([window[(__bridge NSString *)kCGWindowNumber] unsignedIntValue] != id) {
    return SelectedWindowMissing;
  }
  NSNumber *pid = window[(__bridge NSString *)kCGWindowOwnerPID];
  if (owner && !pid) {
    return SelectedWindowUnknown;
  }
  return !owner || pid.intValue == owner ? SelectedWindowPresent : SelectedWindowMissing;
}

static NSString *frameStatusKey;
static NSString *contentRectKey;
static NSString *contentScaleKey;
static NSString *scaleFactorKey;
static NSString *streamErrorDomain;

static NSString *frameworkString(void *framework, const char *symbol) {
  CFStringRef const *value = dlsym(framework, symbol);
  return value ? (__bridge NSString *)*value : nil;
}

int32_t ctream_sck_is_supported(void) {
  if (@available(macOS 14.0, *)) {
    // Keep ScreenCaptureKit out of the viewer's load-time dependencies so the
    // regular viewer can still launch on older macOS releases.
    static void *framework;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
      framework = dlopen("/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit",
                         RTLD_LAZY | RTLD_LOCAL);
      if (framework) {
        frameStatusKey = frameworkString(framework, "SCStreamFrameInfoStatus");
        contentRectKey = frameworkString(framework, "SCStreamFrameInfoContentRect");
        contentScaleKey = frameworkString(framework, "SCStreamFrameInfoContentScale");
        scaleFactorKey = frameworkString(framework, "SCStreamFrameInfoScaleFactor");
        streamErrorDomain = frameworkString(framework, "SCStreamErrorDomain");
      }
    });
    return framework != NULL;
  }
  return 0;
}

int32_t ctream_sck_has_screen_recording_access(void) {
  if (@available(macOS 14.0, *)) {
    return CGPreflightScreenCaptureAccess();
  }
  return 0;
}

static int32_t listWindows(void *context, WindowCallback addWindow, WindowListCallback completion)
    API_AVAILABLE(macos(14.0));

static int32_t listWindows(void *context, WindowCallback addWindow, WindowListCallback completion) {
  if (!NSThread.isMainThread) {
    return CaptureStatusWrongThread;
  }
  if (!ctream_sck_is_supported()) {
    return CaptureStatusUnsupported;
  }
  // Apple's picker can broker access to an individual window. The direct
  // catalog is only available with existing Screen Recording permission.
  if (!ctream_sck_has_screen_recording_access()) {
    return CaptureStatusUnauthorized;
  }
  Class shareableContent = NSClassFromString(@"SCShareableContent");
  if (!shareableContent) {
    return CaptureStatusUnsupported;
  }
  [shareableContent getShareableContentExcludingDesktopWindows:YES
                                           onScreenWindowsOnly:YES
                                             completionHandler:^(SCShareableContent *content, NSError *error) {
    dispatch_async(dispatch_get_main_queue(), ^{
      if (!content || error) {
        completion(context, error.localizedDescription.UTF8String ?: "Could not list macOS windows");
        return;
      }
      pid_t ownPID = NSProcessInfo.processInfo.processIdentifier;
      for (SCWindow *window in content.windows) {
        SCRunningApplication *owner = window.owningApplication;
        if (!owner || owner.processID == ownPID || window.windowLayer != 0 ||
            window.windowID == kCGNullWindowID || CGRectIsEmpty(window.frame)) {
          continue;
        }
        NSString *title = window.title;
        NSString *application = owner.applicationName;
        if (!application.length) {
          application = owner.bundleIdentifier;
        }
        addWindow(context, (__bridge_retained void *)window, window.windowID, owner.processID,
                  title.UTF8String ?: "", application.UTF8String ?: "");
      }
      completion(context, NULL);
    });
  }];
  return CaptureStatusReady;
}

int32_t ctream_sck_list_windows(void *context, WindowCallback addWindow, WindowListCallback completion) {
  if (@available(macOS 14.0, *)) {
    return listWindows(context, addWindow, completion);
  }
  return CaptureStatusUnsupported;
}

void ctream_sck_release_window(void *window) {
  if (NSThread.isMainThread) {
    (void)(__bridge_transfer id)window;
  } else {
    dispatch_async(dispatch_get_main_queue(), ^{ (void)(__bridge_transfer id)window; });
  }
}

API_AVAILABLE(macos(14.0))
@interface CtreamScreenCapture : NSObject <SCContentSharingPickerObserver, SCStreamOutput, SCStreamDelegate>
- (instancetype)initWithContext:(void *)context
                         maxFPS:(int32_t)maxFPS
                          frame:(FrameCallback)frame
                            end:(EndCallback)end
                        release:(ReleaseCallback)release;
- (void)present;
- (void)startWithFilter:(SCContentFilter *)filter window:(SCWindow *)selected;
- (int32_t)windowIdentity:(uint32_t *)id owner:(int32_t *)owner;
- (void)finish:(CaptureEnd)end message:(NSString *)message;
@end

static __weak CtreamScreenCapture *activeCapture API_AVAILABLE(macos(14.0));

// Pack both bounded dimensions so the sample queue can publish them together.
static uint32_t pixelExtent(CGSize size, double scale) {
  double width = round(size.width * scale);
  double height = round(size.height * scale);
  double area = width * height;
  if (!isfinite(area) || width < 1 || height < 1) {
    return 0;
  }
  const double limit = 16 * 1024 * 1024;
  const double maxDimension = 8192;
  double ratio = fmin(1, fmin(sqrt(limit / area), maxDimension / fmax(width, height)));
  width = fmax(1, width * ratio);
  height = fmax(1, height * ratio);
  return ((uint32_t)width << 16) | (uint32_t)height;
}

static CGRect cropToContent(CGRect surface, CGRect content, double scale) {
  double x = CGRectGetMinX(content) * scale;
  double y = CGRectGetMinY(content) * scale;
  double right = CGRectGetMaxX(content) * scale;
  double bottom = CGRectGetMaxY(content) * scale;
  if (!isfinite(x) || !isfinite(y) || !isfinite(right) || !isfinite(bottom)) {
    return surface;
  }
  // Cropping outward can include a row or column of padding during resize.
  // Round inward, tolerating subpixel error in the metadata for integral edges.
  const double tolerance = 0.01;
  x = fmax(CGRectGetMinX(surface), ceil(x - tolerance));
  y = fmax(CGRectGetMinY(surface), ceil(y - tolerance));
  right = fmin(CGRectGetMaxX(surface), floor(right + tolerance));
  bottom = fmin(CGRectGetMaxY(surface), floor(bottom + tolerance));
  if (right <= x || bottom <= y) {
    return surface;
  }
  return CGRectMake(x, y, right - x, bottom - y);
}

static SCStreamConfiguration *configuration(uint32_t extent, int32_t maxFPS) API_AVAILABLE(macos(14.0));

static SCStreamConfiguration *configuration(uint32_t extent, int32_t maxFPS) {
  SCStreamConfiguration *config = [NSClassFromString(@"SCStreamConfiguration") new];
  config.width = extent >> 16;
  config.height = extent & UINT16_MAX;
  config.pixelFormat = kCVPixelFormatType_32BGRA;
  config.minimumFrameInterval = CMTimeMake(1, maxFPS);
  config.queueDepth = 3;
  config.showsCursor = NO;
  config.ignoreShadowsSingleWindow = YES;
  if (@available(macOS 14.2, *)) {
    config.includeChildWindows = NO;
  }
  return config;
}

@implementation CtreamScreenCapture {
  void *_context;
  int32_t _maxFPS;
  FrameCallback _frame;
  EndCallback _end;
  ReleaseCallback _release;
  // Terminal state is shared with the frame queue. Other stream and picker
  // state besides the latest dimensions belongs to the main queue.
  atomic_bool _ended;
  atomic_uint_least32_t _desiredExtent;
  SCContentSharingPicker *_picker;
  SCStream *_stream;
  dispatch_queue_t _frameQueue;
  CGWindowID _windowID;
  pid_t _windowOwner;
  unsigned _missingSamples;
  BOOL _sawWindow;
  dispatch_source_t _captureTimer;
  uint32_t _configuredExtent;
  BOOL _resizeInProgress;
}

- (instancetype)initWithContext:(void *)context
                         maxFPS:(int32_t)maxFPS
                          frame:(FrameCallback)frame
                            end:(EndCallback)end
                        release:(ReleaseCallback)release {
  self = [super init];
  if (self) {
    _context = context;
    _maxFPS = maxFPS;
    _frame = frame;
    _end = end;
    _release = release;
    atomic_init(&_ended, false);
    atomic_init(&_desiredExtent, 0);
    _frameQueue = dispatch_queue_create("ctream.ScreenCaptureKit.frames", DISPATCH_QUEUE_SERIAL);
  }
  return self;
}

- (void)dealloc {
  _release(_context);
}

- (int32_t)windowIdentity:(uint32_t *)id owner:(int32_t *)owner {
  if (!NSThread.isMainThread || _windowID == kCGNullWindowID || _windowOwner <= 0) {
    return 0;
  }
  *id = _windowID;
  *owner = _windowOwner;
  return 1;
}

- (void)present {
  _picker = [NSClassFromString(@"SCContentSharingPicker") sharedPicker];
  SCContentSharingPickerConfiguration *config = [NSClassFromString(@"SCContentSharingPickerConfiguration") new];
  if (!_picker || !config) {
    [self finish:CaptureEndFailed message:@"The macOS window picker is unavailable"];
    return;
  }
  config.allowedPickerModes = SCContentSharingPickerModeSingleWindow;
  config.allowsChangingSelectedContent = NO;
  _picker.defaultConfiguration = config;
  [_picker addObserver:self];
  _picker.active = YES;
  [_picker presentPickerUsingContentStyle:SCShareableContentStyleWindow];
}

- (void)finish:(CaptureEnd)end message:(NSString *)message {
  if (atomic_exchange(&_ended, true)) {
    return;
  }
  if (activeCapture == self) {
    activeCapture = nil;
  }
  [_picker removeObserver:self];
  _picker.active = NO;
  _picker = nil;
  if (_captureTimer) {
    dispatch_source_cancel(_captureTimer);
    _captureTimer = nil;
  }
  SCStream *stream = _stream;
  _stream = nil;
  [stream removeStreamOutput:self type:SCStreamOutputTypeScreen error:NULL];
  [stream stopCaptureWithCompletionHandler:^(NSError *error) {
    // Keep the stream alive until its asynchronous stop completes.
    (void)error;
    (void)stream;
  }];
  if (end != CaptureEndSilent) {
    _end(_context, end, message.UTF8String);
  }
}

- (void)pollWindow {
  SelectedWindowState state = selectedWindowState(_windowID, _windowOwner);
  if (state == SelectedWindowUnknown) {
    return;
  }
  if (state == SelectedWindowPresent) {
    _sawWindow = YES;
    _missingSamples = 0;
  } else if (_sawWindow && ++_missingSamples >= 2) {
    // Only treat missing metadata as closure if it was readable before.
    [self finish:CaptureEndFinished message:nil];
  }
}

- (void)pollCapture {
  if (_windowID != kCGNullWindowID) {
    [self pollWindow];
  }
  if (atomic_load(&_ended) || _resizeInProgress) {
    return;
  }

  uint32_t desired = atomic_load(&_desiredExtent);
  int widthChange = (int)(desired >> 16) - (int)(_configuredExtent >> 16);
  int heightChange = (int)(desired & UINT16_MAX) - (int)(_configuredExtent & UINT16_MAX);
  // Frame metadata can round fractional dimensions differently after scaling.
  if (!desired || (abs(widthChange) <= 2 && abs(heightChange) <= 2)) {
    return;
  }

  _resizeInProgress = YES;
  __weak CtreamScreenCapture *weakSelf = self;
  [_stream updateConfiguration:configuration(desired, _maxFPS) completionHandler:^(NSError *error) {
    dispatch_async(dispatch_get_main_queue(), ^{
      CtreamScreenCapture *capture = weakSelf;
      if (capture) {
        capture->_resizeInProgress = NO;
        if (!error) {
          capture->_configuredExtent = desired;
        }
      }
    });
  }];
}

- (void)startWithFilter:(SCContentFilter *)filter {
  [self startWithFilter:filter window:nil];
}

- (void)startWithFilter:(SCContentFilter *)filter window:(SCWindow *)selected {
  if (atomic_load(&_ended) || _stream) {
    return;
  }
  _configuredExtent = pixelExtent(filter.contentRect.size, filter.pointPixelScale);
  if (filter.style != SCShareableContentStyleWindow || !_configuredExtent) {
    [self finish:CaptureEndFailed message:@"Select a window with a usable capture size"];
    return;
  }
  if (selected) {
    // Direct selection has already verified the window metadata.
    _sawWindow = YES;
  } else if (@available(macOS 15.2, *)) {
    selected = filter.includedWindows.firstObject;
  }
  if (selected) {
    _windowID = selected.windowID;
    _windowOwner = selected.owningApplication.processID;
  }
  _stream = [[NSClassFromString(@"SCStream") alloc] initWithFilter:filter configuration:configuration(_configuredExtent, _maxFPS) delegate:self];
  NSError *error = nil;
  if (![_stream addStreamOutput:self type:SCStreamOutputTypeScreen sampleHandlerQueue:_frameQueue error:&error]) {
    [self finish:CaptureEndFailed message:error.localizedDescription];
    return;
  }
  __weak CtreamScreenCapture *weakSelf = self;
  SCStream *stream = _stream;
  [stream startCaptureWithCompletionHandler:^(NSError *startError) {
    dispatch_async(dispatch_get_main_queue(), ^{
      CtreamScreenCapture *capture = weakSelf;
      if (!capture || atomic_load(&capture->_ended)) {
        // Dropping the viewer while start is pending must not leave a stream
        // active if the earlier stop ran before startup completed.
        if (!startError) {
          [stream stopCaptureWithCompletionHandler:nil];
        }
        return;
      }
      if (startError) {
        [capture finish:CaptureEndFailed message:startError.localizedDescription];
      } else {
        capture->_captureTimer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, dispatch_get_main_queue());
        dispatch_source_set_timer(capture->_captureTimer, DISPATCH_TIME_NOW, 250 * NSEC_PER_MSEC, 50 * NSEC_PER_MSEC);
        dispatch_source_set_event_handler(capture->_captureTimer, ^{ [weakSelf pollCapture]; });
        dispatch_resume(capture->_captureTimer);
      }
    });
  }];
}

- (void)contentSharingPicker:(SCContentSharingPicker *)picker didUpdateWithFilter:(SCContentFilter *)filter forStream:(SCStream *)stream {
  if (!stream) {
    dispatch_async(dispatch_get_main_queue(), ^{ [self startWithFilter:filter]; });
  }
}

- (void)contentSharingPicker:(SCContentSharingPicker *)picker didCancelForStream:(SCStream *)stream {
  if (!stream) {
    dispatch_async(dispatch_get_main_queue(), ^{ [self finish:CaptureEndCancelled message:nil]; });
  }
}

- (void)contentSharingPickerStartDidFailWithError:(NSError *)error {
  dispatch_async(dispatch_get_main_queue(), ^{ [self finish:CaptureEndFailed message:error.localizedDescription]; });
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
  dispatch_async(dispatch_get_main_queue(), ^{
    // Apple's frame selector can stop a session explicitly from Control Center.
    BOOL normal = [error.domain isEqualToString:streamErrorDomain] &&
        (error.code == SCStreamErrorUserStopped || error.code == SCStreamErrorNoCaptureSource);
    [self finish:normal ? CaptureEndFinished : CaptureEndFailed message:error.localizedDescription];
  });
}

- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sample ofType:(SCStreamOutputType)type {
  CtreamScreenCapture *owner __attribute__((objc_precise_lifetime)) = self;
  if (atomic_load(&owner->_ended) || type != SCStreamOutputTypeScreen || !CMSampleBufferIsValid(sample)) {
    return;
  }
  CFArrayRef attachments = CMSampleBufferGetSampleAttachmentsArray(sample, false);
  NSDictionary *metadata = [(__bridge NSArray *)attachments firstObject];
  NSNumber *status = frameStatusKey ? metadata[frameStatusKey] : nil;
  if (status && status.integerValue != SCFrameStatusComplete && status.integerValue != SCFrameStatusStarted) {
    return;
  }
  CVImageBufferRef image = CMSampleBufferGetImageBuffer(sample);
  if (!image || CVPixelBufferGetPixelFormatType(image) != kCVPixelFormatType_32BGRA ||
      CVPixelBufferLockBaseAddress(image, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess) {
    return;
  }
  size_t width = CVPixelBufferGetWidth(image);
  size_t height = CVPixelBufferGetHeight(image);
  CGRect crop = CGRectMake(0, 0, width, height);
  if (contentRectKey && contentScaleKey && scaleFactorKey) {
    NSDictionary *rect = metadata[contentRectKey];
    double backingScale = [metadata[scaleFactorKey] doubleValue];
    double contentScale = [metadata[contentScaleKey] doubleValue];
    CGRect content;
    if (rect && backingScale > 0 && isfinite(backingScale) &&
        CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)rect, &content)) {
      // The frame can be padded while the surface retains its old size. Use
      // the visible pixels now, then request the unscaled size on the main queue.
      crop = cropToContent(crop, content, backingScale);
      uint32_t desired = contentScale > 0 ? pixelExtent(content.size, backingScale / contentScale) : 0;
      if (desired) {
        atomic_store(&owner->_desiredExtent, desired);
      }
    }
  }
  const uint8_t *bytes = CVPixelBufferGetBaseAddress(image);
  size_t len = CVPixelBufferGetDataSize(image);
  size_t stride = CVPixelBufferGetBytesPerRow(image);
  size_t x = (size_t)crop.origin.x;
  size_t y = (size_t)crop.origin.y;
  if (bytes && stride && width <= stride / 4 && y <= len / stride) {
    size_t row = y * stride;
    size_t column = x * 4;
    if (column < len - row && !atomic_load(&owner->_ended)) {
      size_t offset = row + column;
      owner->_frame(owner->_context, bytes + offset, len - offset,
                    (size_t)crop.size.width, (size_t)crop.size.height, stride);
    }
  }
  CVPixelBufferUnlockBaseAddress(image, kCVPixelBufferLock_ReadOnly);
}

@end

void *ctream_sck_start(void *window, int32_t maxFPS, int32_t independent, void *context, FrameCallback frame, EndCallback end, ReleaseCallback release, int32_t *status) {
  if (@available(macOS 14.0, *)) {
    if (!NSThread.isMainThread) {
      *status = CaptureStatusWrongThread;
      return NULL;
    }
    // The picker and retained callers share one slot. An explicitly selected
    // leased window owns a separate SCStream and never registers a picker.
    BOOL leasedWindow = window != NULL && independent;
    if (!leasedWindow && activeCapture) {
      *status = CaptureStatusOccupied;
      return NULL;
    }
    if (ctream_sck_is_supported()) {
      SCWindow *selected = (__bridge SCWindow *)window;
      if (selected && !ctream_sck_has_screen_recording_access()) {
        *status = CaptureStatusUnauthorized;
        return NULL;
      }
      if (selected && selectedWindowState(selected.windowID, selected.owningApplication.processID) != SelectedWindowPresent) {
        *status = CaptureStatusWindowUnavailable;
        return NULL;
      }
      CtreamScreenCapture *capture = [[CtreamScreenCapture alloc] initWithContext:context maxFPS:maxFPS frame:frame end:end release:release];
      if (!leasedWindow) {
        activeCapture = capture;
      }
      if (selected) {
        SCContentFilter *filter = [[NSClassFromString(@"SCContentFilter") alloc] initWithDesktopIndependentWindow:selected];
        [capture startWithFilter:filter window:selected];
      } else {
        [capture present];
      }
      return (__bridge_retained void *)capture;
    }
  }
  *status = CaptureStatusUnsupported;
  return NULL;
}

int32_t ctream_sck_window_identity(void *handle, uint32_t *id, int32_t *owner) {
  if (@available(macOS 14.0, *)) {
    CtreamScreenCapture *capture = (__bridge CtreamScreenCapture *)handle;
    return [capture windowIdentity:id owner:owner];
  }
  return 0;
}

void ctream_sck_stop(void *handle) {
  if (@available(macOS 14.0, *)) {
    CtreamScreenCapture *capture = (__bridge_transfer CtreamScreenCapture *)handle;
    [capture finish:CaptureEndSilent message:nil];
  }
}
