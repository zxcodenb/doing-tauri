use super::*;
use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::*;
use std::sync::mpsc;
use std::time::Duration;

// All fallible Objective-C entry points, including asynchronously invoked blocks, pass
// through this seam. Error details must not expose the OS exception or notification text.
fn native_call<T>(operation: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    // A Rust panic catcher alone cannot catch NSException. Catch Objective-C first and
    // Rust panics outside it, so neither can escape an OS delegate/block callback. Calls
    // are abandoned on failure; no partially computed result or raw exception is reused.
    catch_unwind(AssertUnwindSafe(|| {
        objc2::exception::catch(AssertUnwindSafe(operation)).unwrap_or(Err(Error::Unavailable))
    }))
    .unwrap_or(Err(Error::Unavailable))
}

struct DelegateIvars {
    activated: Activated,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "DoingNotificationDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            done: &DynBlock<dyn Fn()>,
        ) {
            // Do not unwind across Objective-C. Only opaque IDs reach the app callback.
            let _ = native_call(|| {
                let action = response.actionIdentifier();
                if &*action == unsafe { UNNotificationDefaultActionIdentifier } {
                    let identifier = response.notification().request().identifier().to_string();
                    (self.ivars().activated)(activation_from_identifier(&identifier));
                }
                Ok(())
            });
            let _ = native_call(|| {
                done.call(());
                Ok(())
            });
        }
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            done: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            let _ = native_call(|| {
                done.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List
                    | UNNotificationPresentationOptions::Sound,));
                Ok(())
            });
        }
    }
);
// The delegate has only an immutable Send+Sync Rust callback; the OS can call its methods
// on its notification queue. It never accesses AppKit/UI objects or mutates Objective-C ivars.
unsafe impl Send for Delegate {}
unsafe impl Sync for Delegate {}
impl Delegate {
    fn new(activated: Activated) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { activated });
        unsafe { msg_send![super(this), init] }
    }
}
pub(super) struct Native {
    _delegate: Retained<Delegate>,
}
impl Native {
    pub fn new(identity: Identity, activated: Activated) -> Result<Self, Error> {
        native_call(|| {
            // UNUserNotificationCenter can throw for an unbundled executable. Never impersonate
            // Terminal (the old plugin's dev fallback) or inspect another app's notification state.
            if NSBundle::mainBundle()
                .bundleIdentifier()
                .as_deref()
                .map(NSString::to_string)
                .as_deref()
                != Some(&identity.application_id)
            {
                return Err(Error::BundleRequired);
            }
            let delegate = Delegate::new(activated);
            UNUserNotificationCenter::currentNotificationCenter()
                .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            Ok(Self {
                _delegate: delegate,
            })
        })
    }
    pub fn permission(&self, request: bool) -> Result<Permission, Error> {
        native_call(|| {
            let center = UNUserNotificationCenter::currentNotificationCenter();
            if request {
                let (tx, rx) = mpsc::sync_channel(1);
                let callback = RcBlock::new(move |granted: Bool, error: *mut NSError| {
                    let _ = tx.try_send(if !error.is_null() {
                        Err(Error::Unavailable)
                    } else {
                        Ok(granted.as_bool())
                    });
                });
                center.requestAuthorizationWithOptions_completionHandler(
                    UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                    &callback,
                );
                // A user-facing permission prompt may take longer than a submission. A timeout
                // is not a denial; a later query reads the actual OS state.
                rx.recv_timeout(Duration::from_secs(120))
                    .map_err(|_| Error::Timeout)??;
            }
            let (tx, rx) = mpsc::sync_channel(1);
            let callback =
                RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
                    let permission = native_call(|| {
                        // SAFETY: the OS block contract supplies a live non-null settings object.
                        match unsafe { settings.as_ref() }.authorizationStatus() {
                            UNAuthorizationStatus::Authorized
                            | UNAuthorizationStatus::Provisional
                            | UNAuthorizationStatus::Ephemeral => Ok(Permission::Granted),
                            UNAuthorizationStatus::Denied => Ok(Permission::Denied),
                            UNAuthorizationStatus::NotDetermined => Ok(Permission::NotDetermined),
                            _ => Err(Error::Unavailable),
                        }
                    });
                    let _ = tx.try_send(permission);
                });
            center.getNotificationSettingsWithCompletionHandler(&callback);
            rx.recv_timeout(Duration::from_secs(10))
                .map_err(|_| Error::Timeout)?
        })
    }
    pub fn submit(&self, request: Request<'_>) -> Result<(), Error> {
        native_call(|| {
            if self.permission(false)? != Permission::Granted {
                return Err(Error::PermissionDenied);
            }
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(request.title));
            content.setBody(&NSString::from_str(request.body));
            if request.sound {
                content.setSound(Some(&UNNotificationSound::defaultSound()));
            }
            let id = NSString::from_str(&format!("doing-due-{}", request.id));
            let request =
                UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
            let (tx, rx) = mpsc::sync_channel(1);
            let completion = RcBlock::new(move |error: *mut NSError| {
                let _ = tx.try_send(if error.is_null() {
                    Ok(())
                } else {
                    Err(Error::Unavailable)
                });
            });
            UNUserNotificationCenter::currentNotificationCenter()
                .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
            rx.recv_timeout(Duration::from_secs(10))
                .map_err(|_| Error::Timeout)?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_exception_is_redacted_as_unavailable() {
        // Throw only a synthetic in-memory object, never call the notification center or
        // prompt for authorization. The outer catch contains the regression even when the
        // inner production guard is absent, so this test cannot abort the test runner.
        let result = objc2::exception::catch(|| {
            native_call::<()>(|| {
                let object = NSObject::new();
                // SAFETY: Objective-C allows throwing any retained object; Exception is an
                // opaque AnyObject wrapper and is released by the matching native catch.
                let exception = unsafe { Retained::cast_unchecked(object) };
                objc2::exception::throw(exception)
            })
        });
        assert!(matches!(result, Ok(Err(Error::Unavailable))));
    }

    #[test]
    fn native_guard_contains_rust_panics_before_ffi_return() {
        assert_eq!(
            native_call::<()>(|| panic!("synthetic callback panic")),
            Err(Error::Unavailable)
        );
    }

    #[test]
    fn native_guard_preserves_normal_results_and_public_errors() {
        assert_eq!(native_call(|| Ok(17)), Ok(17));
        assert_eq!(
            native_call::<()>(|| Err(Error::PermissionDenied)),
            Err(Error::PermissionDenied)
        );
    }
}
