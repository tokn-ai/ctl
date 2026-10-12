//! Owned, serialized public `LocalAuthentication` evaluation for the manual probe.

use super::{
  AuthenticationContext, Error, INTERACTION_NOT_ALLOWED, NativeError, REASON, ReadResult,
};
use block2::RcBlock;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use objc2::encode::{Encoding, RefEncode};
use objc2::msg_send;
use objc2::runtime::{AnyObject, Bool};
use security_framework::access_control::SecAccessControl;
use std::sync::mpsc;
use std::time::Duration;

const EVALUATION_DEADLINE: Duration = Duration::from_secs(180);
const LA_ACCESS_CONTROL_USE_ITEM: isize = 1;

// security-framework-sys's opaque CF type has no Objective-C encoding. This
// marker describes the SDK's SecAccessControlRef argument; it is never created,
// dereferenced, or used to reinterpret the native object's layout.
#[repr(C)]
struct AccessControlArgument {
  _opaque: [u8; 0],
}

// SAFETY: clang's @encode(SecAccessControlRef) from the installed public SDK is
// `^{__SecAccessControl=}`. Only pointers to the marker cross this call boundary.
unsafe impl RefEncode for AccessControlArgument {
  const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__SecAccessControl", &[]));
}

pub(super) enum NativeEvaluation {
  AccessControl(SecAccessControl),
  Policy(isize),
}

impl NativeEvaluation {
  fn stage(&self) -> &'static str {
    match self {
      Self::AccessControl(_) => "native_access_control_evaluation",
      Self::Policy(_) => "native_policy_evaluation",
    }
  }
}

pub(super) fn evaluate_native(
  context: &AuthenticationContext,
  evaluation: &NativeEvaluation,
  allow_ui: bool,
) -> ReadResult {
  let _authorization = match context.authorize(REASON, allow_ui) {
    Ok(guard) => guard,
    Err(error) => return ReadResult::failure("native_precheck", error),
  };
  let (sender, receiver) = mpsc::sync_channel(1);
  let callback_control = match evaluation {
    NativeEvaluation::AccessControl(control) => Some(control.clone()),
    NativeEvaluation::Policy(_) => None,
  };
  let reply = RcBlock::new(move |success: Bool, error: *mut AnyObject| {
    // The framework retains this block for its asynchronous reply. Keeping the
    // exact ACL in the capture preserves ownership even after our timeout.
    let _control = &callback_control;
    let native_error = (!error.is_null()).then(|| {
      // SAFETY: the reply supplies a borrowed NSError valid for the callback;
      // code is NSInteger and domain is an NSString toll-free bridged to CF.
      let code: isize = unsafe { msg_send![error, code] };
      let domain: *const AnyObject = unsafe { msg_send![error, domain] };
      let domain = if domain.is_null() {
        "unknown_native_error_domain".into()
      } else {
        // SAFETY: Get-rule retain owns the live NSString while copying only its
        // error-domain identifier. Descriptions and userInfo are never read.
        unsafe { CFString::wrap_under_get_rule(domain.cast()) }
          .to_string()
          .chars()
          .take(512)
          .collect()
      };
      NativeError { domain, code }
    });
    let _ = sender.send((success.as_bool(), native_error));
  });
  let reason = CFString::new(REASON);
  // SAFETY: these are the public LAContext signatures and SDK enum values.
  // CFString is toll-free NSString, the context/ACL stay owned, and block2
  // supplies the typed BOOL/NSError callback ABI. The guard serializes context
  // configuration and the entire asynchronous evaluation.
  unsafe {
    match evaluation {
      NativeEvaluation::AccessControl(control) => {
        let _: () = msg_send![&*context.context,
          evaluateAccessControl: control.as_concrete_TypeRef().cast::<AccessControlArgument>(),
          operation: LA_ACCESS_CONTROL_USE_ITEM,
          localizedReason: reason.as_concrete_TypeRef().cast::<AnyObject>(),
          reply: &*reply
        ];
      }
      NativeEvaluation::Policy(policy) => {
        let _: () = msg_send![&*context.context,
          evaluatePolicy: *policy,
          localizedReason: reason.as_concrete_TypeRef().cast::<AnyObject>(),
          reply: &*reply
        ];
      }
    }
  }
  match receiver.recv_timeout(EVALUATION_DEADLINE) {
    Ok((true, None)) if !context.is_invalidated() => ReadResult::native_success(),
    Ok((true, None)) => ReadResult::failure("native_postcheck", Error(INTERACTION_NOT_ALLOWED)),
    Ok((_, native_error)) => ReadResult {
      stage: evaluation.stage(),
      status: None,
      native_error,
      succeeded: false,
    },
    Err(_) => {
      // Invalidate cancels outstanding evaluation without taking this guard,
      // so timeout and external cancellation cannot deadlock the operation.
      context.invalidate();
      ReadResult {
        stage: "native_evaluation_callback_timeout",
        status: None,
        native_error: None,
        succeeded: false,
      }
    }
  }
}
