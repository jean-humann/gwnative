//! Observing Core Foundation notifications without owning an Objective-C class.
//!
//! Three modules here want to be told when something outside their control
//! changes: the window's frame, the application's focus, the keyboard layout.
//! AppKit posts to the default `NSNotificationCenter`, which is Core
//! Foundation's local centre — the two are bridged — and none of the observers
//! needs an object, only a function. So this reads them as C callbacks rather
//! than declaring a class purely to own a selector.
//!
//! Nothing can be unregistered. Every observer here is wanted for as long as the
//! process runs, and an observer that can be removed needs an owner to remove
//! it — which is the class this exists to avoid.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2_foundation::NSString;

pub type CenterRef = *const c_void;

/// The shape Core Foundation calls back with.
///
/// Every observer in this build ignores all five arguments: it registered for
/// one notification, so the fact of the call is the whole message.
pub type Callback = extern "C" fn(
    center: CenterRef,
    observer: *mut c_void,
    name: *const c_void,
    object: *const c_void,
    user_info: *const c_void,
);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    /// This process's own notifications, which is where AppKit posts.
    fn CFNotificationCenterGetLocalCenter() -> CenterRef;
    /// Notifications between applications, which is where the input source
    /// change arrives from.
    fn CFNotificationCenterGetDistributedCenter() -> CenterRef;
    fn CFNotificationCenterAddObserver(
        center: CenterRef,
        observer: *const c_void,
        callback: Callback,
        name: *const c_void,
        object: *const c_void,
        suspension_behavior: i32,
    );
}

/// Deliver even while the application is inactive, rather than coalescing until
/// it comes forward. Only meaningful on the distributed centre.
pub const DELIVER_IMMEDIATELY: i32 = 4;

/// Local notifications are never suspended, so the behaviour is moot; 0 is
/// `CFNotificationSuspensionBehaviorDrop`, which the local centre ignores.
const SUSPENSION_UNUSED: i32 = 0;

/// Call `callback` whenever this process posts `name`.
pub fn local(name: &str, callback: Callback) {
    let name = NSString::from_str(name);
    // SAFETY: `NSString` is toll-free bridged to `CFStringRef`. The name is
    // deliberately leaked, because the observer outlives every scope that could
    // free it. The observer pointer is null and is handed back only to
    // callbacks that ignore it.
    unsafe {
        let name = Retained::into_raw(name).cast::<c_void>();
        add(
            CFNotificationCenterGetLocalCenter(),
            name,
            callback,
            SUSPENSION_UNUSED,
        );
    }
}

/// Call `callback` whenever any application posts `name`.
///
/// The name is a `CFStringRef` the caller already holds — these notifications
/// are named by framework constants, not by strings this build spells — so it
/// is passed through rather than built here.
///
/// # Safety
///
/// `name` must be a live `CFStringRef`.
pub unsafe fn distributed(name: *const c_void, callback: Callback, suspension: i32) {
    unsafe {
        add(
            CFNotificationCenterGetDistributedCenter(),
            name,
            callback,
            suspension,
        )
    };
}

/// # Safety
///
/// `center` must be a centre this process may observe, and `name` a live
/// `CFStringRef` that outlives the observer — which, since nothing here ever
/// removes one, means for the rest of the process.
unsafe fn add(center: CenterRef, name: *const c_void, callback: Callback, suspension: i32) {
    unsafe {
        CFNotificationCenterAddObserver(
            center,
            std::ptr::null(),
            callback,
            name,
            std::ptr::null(),
            suspension,
        );
    }
}
