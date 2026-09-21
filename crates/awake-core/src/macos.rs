//! macOS backend — IOKit power assertions.
//!
//! Mirrors what every app in this category does. Deliberately
//! calls `IOPMAssertionCreateWithProperties` rather than the simpler
//! `IOPMAssertionCreateWithName`, because only the properties form accepts
//! `kIOPMAssertionTimeoutKey` — and an OS-enforced deadline is the only timeout that
//! survives our process being suspended or killed.
//!
//! We never shell out to `/usr/bin/caffeinate`: it is a subprocess to babysit, it is
//! blocked under App Sandbox, and it is a thin wrapper over exactly these calls.

use std::sync::Mutex;
use std::time::Duration;

use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;

use crate::{Backend, Error, Flags, Handle, Request, Result};

// Assertion type strings. These are the modern names; `pmset -g assertions` may print
// the legacy aliases (`NoDisplaySleepAssertion` / `NoIdleSleepAssertion`) instead —
// that is a display quirk, not a different assertion.
const TYPE_DISPLAY: &str = "PreventUserIdleDisplaySleep";
const TYPE_SYSTEM: &str = "PreventUserIdleSystemSleep";
const TYPE_DISK: &str = "PreventDiskIdle";

// Property dictionary keys, from IOKit/pwr_mgt/IOPMLib.h.
const KEY_TYPE: &str = "AssertType";
const KEY_LEVEL: &str = "AssertLevel";
const KEY_NAME: &str = "AssertName";
const KEY_TIMEOUT: &str = "TimeoutSeconds";
const KEY_TIMEOUT_ACTION: &str = "TimeoutAction";
const TIMEOUT_ACTION_RELEASE: &str = "TimeoutActionRelease";

const LEVEL_ON: i32 = 255;
const IOPM_USER_ACTIVE_LOCAL: i32 = 0;
const KERN_SUCCESS: i32 = 0;

/// `kIOReturnBadArgument` (0xE00002C2) as a signed `IOReturn`.
///
/// This is what `IOPMAssertionRelease` returns for an id it does not recognise.
/// Measured on macOS 26, because it is not what the obvious guess would be — the
/// three interesting cases all return *this*, not `kIOReturnNotFound`:
///
/// ```text
/// release(bogus id)             -> 0xE00002C2
/// release(already released)     -> 0xE00002C2
/// release(after kernel timeout) -> 0xE00002C2
/// ```
///
/// Because a benign double-release is indistinguishable from a caller passing
/// garbage, we do **not** tolerate this unconditionally — see [`Handle::timed`].
const IO_RETURN_BAD_ARGUMENT: i32 = -536870206;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPMAssertionCreateWithProperties(
        assertion_properties: CFDictionaryRef,
        assertion_id: *mut u32,
    ) -> i32;

    fn IOPMAssertionRelease(assertion_id: u32) -> i32;

    fn IOPMAssertionDeclareUserActivity(
        assertion_name: CFStringRef,
        user_type: i32,
        assertion_id: *mut u32,
    ) -> i32;
}

#[derive(Debug, Default)]
pub struct IoKitBackend {
    /// Reused across `declare_user_activity` calls.
    ///
    /// `IOPMAssertionDeclareUserActivity` *re-arms* an existing id when given one, but
    /// creates a brand new assertion when given 0. Passing 0 every tick would leak one
    /// assertion per call — which, on a 30s screensaver tick, is 2,880 leaked
    /// assertions a day.
    user_activity_id: Mutex<u32>,
}

fn create_assertion(kind: &str, name: &str, timeout: Option<Duration>) -> Result<u32> {
    let mut pairs: Vec<(CFType, CFType)> = vec![
        (CFString::new(KEY_TYPE).as_CFType(), CFString::new(kind).as_CFType()),
        (CFString::new(KEY_LEVEL).as_CFType(), CFNumber::from(LEVEL_ON).as_CFType()),
        (CFString::new(KEY_NAME).as_CFType(), CFString::new(name).as_CFType()),
    ];

    if let Some(t) = timeout {
        pairs.push((
            CFString::new(KEY_TIMEOUT).as_CFType(),
            CFNumber::from(t.as_secs_f64()).as_CFType(),
        ));
        pairs.push((
            CFString::new(KEY_TIMEOUT_ACTION).as_CFType(),
            CFString::new(TIMEOUT_ACTION_RELEASE).as_CFType(),
        ));
    }

    let dict = CFDictionary::from_CFType_pairs(&pairs);
    let mut id: u32 = 0;
    // SAFETY: `dict` outlives the call; `id` is a valid out-pointer. IOKit copies what
    // it needs from the dictionary before returning.
    let rc = unsafe { IOPMAssertionCreateWithProperties(dict.as_concrete_TypeRef(), &mut id) };

    if rc != KERN_SUCCESS {
        return Err(Error::Os { call: "IOPMAssertionCreateWithProperties", code: rc as i64 });
    }
    Ok(id)
}

fn kinds_for(flags: Flags) -> Vec<&'static str> {
    let mut v = Vec::with_capacity(3);
    if flags.display {
        v.push(TYPE_DISPLAY);
    }
    if flags.system {
        v.push(TYPE_SYSTEM);
    }
    if flags.disk {
        v.push(TYPE_DISK);
    }
    v
}

impl Backend for IoKitBackend {
    fn name(&self) -> &'static str {
        "macos/iokit"
    }

    fn acquire(&self, req: &Request) -> Result<Handle> {
        // Prefixed so the user can identify us in `pmset -g assertions`, which is how
        // people debug "why won't my Mac sleep".
        let name = format!("no-afk: {}", req.reason);
        let mut ids: Vec<u64> = Vec::new();

        for kind in kinds_for(req.flags) {
            match create_assertion(kind, &name, req.timeout) {
                Ok(id) => ids.push(u64::from(id)),
                Err(err) => {
                    // Roll back. A partially-acquired session is worse than a failed
                    // one: the UI would show "off" while assertions were still held,
                    // and nothing would ever release them.
                    for id in &ids {
                        unsafe { IOPMAssertionRelease(*id as u32) };
                    }
                    return Err(err);
                }
            }
        }

        Ok(Handle { ids, timed: req.timeout.is_some() })
    }

    fn release(&self, handle: &Handle) -> Result<()> {
        let mut first_err = None;
        // Attempt every id even after a failure — stopping early would leak the rest.
        for id in &handle.ids {
            let rc = unsafe { IOPMAssertionRelease(*id as u32) };
            if rc == KERN_SUCCESS {
                continue;
            }
            // For a timed assertion, BAD_ARGUMENT just means the kernel beat us to it.
            // For an untimed one, nothing should have released it but us, so the same
            // code indicates a genuine bug and must surface.
            if rc == IO_RETURN_BAD_ARGUMENT && handle.timed {
                continue;
            }
            if first_err.is_none() {
                first_err = Some(Error::Os { call: "IOPMAssertionRelease", code: rc as i64 });
            }
        }
        match first_err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn declare_user_activity(&self, reason: &str) -> Result<()> {
        let name = CFString::new(&format!("no-afk: {reason}"));
        let mut guard = self.user_activity_id.lock().unwrap_or_else(|e| e.into_inner());
        let mut id: u32 = *guard;

        let rc = unsafe {
            IOPMAssertionDeclareUserActivity(
                name.as_concrete_TypeRef(),
                IOPM_USER_ACTIVE_LOCAL,
                &mut id,
            )
        };

        if rc != KERN_SUCCESS {
            return Err(Error::Os { call: "IOPMAssertionDeclareUserActivity", code: rc as i64 });
        }
        *guard = id;
        Ok(())
    }

    // TODO(phase-2): implement via `IOPMCopyAssertionsByProcess`, which returns
    // CFDictionary<CFNumber pid, CFArray<CFDictionary>>. Backs the "why is my Mac
    // awake?" panel. Left unimplemented rather than faked so
    // callers get a clear Unsupported instead of a plausible empty list.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{acquire, Request};
    use std::sync::Arc;

    #[test]
    fn kinds_map_one_assertion_per_flag() {
        assert_eq!(kinds_for(Flags::display_and_system()).len(), 2);
        assert_eq!(kinds_for(Flags::system_only()), vec![TYPE_SYSTEM]);
        assert!(kinds_for(Flags::default()).is_empty());
    }

    /// Exercises the real IOKit path end to end. Safe to run in CI on macOS: it holds
    /// assertions for microseconds and releases them before returning.
    #[test]
    fn real_assertions_round_trip() {
        let backend = Arc::new(IoKitBackend::default());
        let req = Request::new(Flags::display_and_system(), "unit test");

        let guard = acquire(backend, req).expect("acquire should succeed on macOS");
        assert_eq!(guard.handle().ids.len(), 2, "display + system = two assertions");
        assert!(guard.handle().ids.iter().all(|id| *id != 0), "ids should be non-zero");

        guard.release().expect("release should succeed");
    }

    #[test]
    fn timeout_assertion_is_accepted_by_the_kernel() {
        let backend = IoKitBackend::default();
        let req = Request::new(Flags::system_only(), "timeout test")
            .with_timeout(Duration::from_secs(1));

        let handle = backend.acquire(&req).expect("timed acquire should succeed");
        backend.release(&handle).expect("release should succeed");
    }

    /// Regression: a timed session that the kernel expires must still release cleanly.
    /// Before this was handled, every successfully-expired session logged a spurious
    /// "FAILED to release" on drop.
    #[test]
    fn releasing_a_timed_assertion_twice_is_ok() {
        let backend = IoKitBackend::default();
        let req = Request::new(Flags::system_only(), "timed")
            .with_timeout(Duration::from_secs(60));

        let handle = backend.acquire(&req).expect("acquire");
        assert!(handle.timed);

        backend.release(&handle).expect("first release");
        backend
            .release(&handle)
            .expect("second release must be tolerated — the kernel may have got there first");
    }

    /// The flip side: nothing but us can release an *untimed* assertion, so a
    /// double-release is a real bug and must not be silently swallowed.
    #[test]
    fn releasing_an_untimed_assertion_twice_is_an_error() {
        let backend = IoKitBackend::default();
        let handle = backend
            .acquire(&Request::new(Flags::system_only(), "untimed"))
            .expect("acquire");
        assert!(!handle.timed);

        backend.release(&handle).expect("first release");
        let err = backend.release(&handle).expect_err("second release should surface");
        assert!(matches!(err, Error::Os { call: "IOPMAssertionRelease", .. }));
    }

    #[test]
    fn declare_user_activity_reuses_its_id() {
        let backend = IoKitBackend::default();
        backend.declare_user_activity("test").expect("first call");
        let first = *backend.user_activity_id.lock().unwrap();

        backend.declare_user_activity("test").expect("second call");
        let second = *backend.user_activity_id.lock().unwrap();

        assert_ne!(first, 0, "should have stored a real assertion id");
        assert_eq!(first, second, "must re-arm, not create a second assertion");
    }
}
