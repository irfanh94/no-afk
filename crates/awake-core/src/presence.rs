//! L3 — keeping presence detectors (Slack, Teams) from marking you away.
//!
//! This is a different mechanism from the rest of the crate and carries a different
//! cost. Power assertions are free and permission-less; this needs an **Accessibility
//! grant** and only works by posting synthetic input.
//!
//! # Why it has to work this way
//!
//! Measured, not assumed. Presence detectors read the HID idle counter —
//! `CGEventSourceSecondsSinceLastEventType`, which is what Electron's
//! `powerMonitor.getSystemIdleTime` returns, and both Slack and Teams are Electron.
//! Three candidate ways to reset it were tested:
//!
//! | Approach | Needs permission | Resets the counter |
//! |---|---|---|
//! | `IOPMAssertionDeclareUserActivity` | no | **no** |
//! | `CGWarpMouseCursorPosition` | no | **no** |
//! | `CGEventPost` of a key | Accessibility | yes, once granted |
//!
//! The first two are clean negative results: both were called successfully and the
//! counter carried on climbing straight through them. So there is no permission-free
//! path, and Slack's own API cannot help either — `users.setPresence` accepts only
//! `auto` or `away`, never "force active".
//!
//! # Guard rails
//!
//! Synthetic input is intrusive, so [`should_nudge`] keeps it rare and invisible:
//!
//! - Never fires unless the machine has *already* been idle past a threshold, so it
//!   cannot land in the middle of real typing or a drag.
//! - Never fires while the screen is locked — resetting the idle timer behind a lock
//!   screen serves no purpose and would misreport a genuinely absent user.
//! - Never fires without the grant; [`nudge`] fails loudly instead of silently
//!   no-opping, because an ungranted `CGEventPost` is discarded by the OS with no
//!   error of its own.

use std::time::Duration;

use crate::{Error, Result};

/// How long the machine must have been idle before a nudge is allowed.
///
/// Slack marks you away after about 10 minutes and Teams after about 5, so two
/// minutes leaves generous margin while keeping injections infrequent — roughly 30 an
/// hour while you are away, and none at all while you are actually using the machine.
pub const DEFAULT_IDLE_THRESHOLD: Duration = Duration::from_secs(120);

/// The whole decision, as a pure function.
///
/// Kept separate from the FFI so the interesting part — when we are and aren't
/// allowed to inject — is testable without an Accessibility grant, a display, or a
/// real idle clock.
pub fn should_nudge(
    trusted: bool,
    screen_locked: bool,
    idle: Duration,
    threshold: Duration,
) -> bool {
    trusted && !screen_locked && idle >= threshold
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn is_trusted() -> bool {
        false
    }
    pub fn request_trust() {}
    pub fn idle() -> Duration {
        Duration::ZERO
    }
    pub fn screen_locked() -> bool {
        false
    }
    pub fn post_nudge() -> Result<()> {
        Err(Error::Unsupported("presence nudging is macOS-only so far"))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    use std::os::raw::c_void;

    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{CFGetTypeID, CFRelease, CFTypeRef};
    use core_foundation_sys::dictionary::{CFDictionaryGetValueIfPresent, CFDictionaryRef};

    // Opaque CoreGraphics handles.
    type CGEventSourceRef = *const c_void;
    type CGEventRef = *const c_void;

    const HID_SYSTEM_STATE: i32 = 1; // kCGEventSourceStateHIDSystemState
    const HID_EVENT_TAP: u32 = 0; // kCGHIDEventTap
    const ANY_INPUT_EVENT_TYPE: u32 = u32::MAX; // kCGAnyInputEventType

    /// F15. Chosen because essentially nothing binds it, so the keystroke has no
    /// visible effect — the traditional choice for this trick.
    const VK_F15: u16 = 0x71;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;
        fn CGEventCreateKeyboardEvent(
            source: CGEventSourceRef,
            virtual_key: u16,
            key_down: bool,
        ) -> CGEventRef;
        fn CGEventPost(tap: u32, event: CGEventRef);
        fn CGEventSourceSecondsSinceLastEventType(state_id: i32, event_type: u32) -> f64;
        /// Returns NULL when there is no window server session (headless / ssh).
        fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
    }

    pub fn is_trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    /// Ask for the grant, which makes macOS show its Accessibility prompt.
    ///
    /// Returns nothing useful: the user has to visit System Settings and the answer
    /// only shows up in a later [`is_trusted`] call, not here.
    pub fn request_trust() {
        // The literal value of kAXTrustedCheckOptionPrompt. Used directly rather than
        // linking the global, which is awkward to bind and identical in effect.
        let key = CFString::new("AXTrustedCheckOptionPrompt");
        let options = CFDictionary::from_CFType_pairs(&[(
            key.as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);
        unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) };
    }

    /// Idle time exactly as Slack and Teams see it.
    pub fn idle() -> Duration {
        let secs = unsafe {
            CGEventSourceSecondsSinceLastEventType(HID_SYSTEM_STATE, ANY_INPUT_EVENT_TYPE)
        };
        // Guards against a negative or NaN reading becoming a gigantic Duration.
        if secs.is_finite() && secs > 0.0 {
            Duration::from_secs_f64(secs)
        } else {
            Duration::ZERO
        }
    }

    pub fn screen_locked() -> bool {
        // SAFETY: returns NULL with no session; otherwise owned under the create rule.
        let dict = unsafe { CGSessionCopyCurrentDictionary() };
        if dict.is_null() {
            return false;
        }

        let key = CFString::new("CGSSessionScreenIsLocked");
        let mut value: *const c_void = std::ptr::null();
        let locked = unsafe {
            if CFDictionaryGetValueIfPresent(dict, key.as_CFTypeRef() as *const c_void, &mut value)
                != 0
                && !value.is_null()
                && CFGetTypeID(value as CFTypeRef) == CFBoolean::type_id()
            {
                CFBoolean::wrap_under_get_rule(value as _).into()
            } else {
                // Key is absent entirely when unlocked, which is the common case.
                false
            }
        };

        unsafe { CFRelease(dict as CFTypeRef) };
        locked
    }

    pub fn post_nudge() -> Result<()> {
        // Checked rather than assumed: an ungranted CGEventPost is silently dropped
        // by the OS, so without this the caller could not tell success from a no-op.
        if !is_trusted() {
            return Err(Error::Unsupported(
                "Accessibility permission has not been granted",
            ));
        }

        // SAFETY: a NULL source is valid to CoreGraphics (it means "no source"), and
        // every event created here is released before returning.
        unsafe {
            let source = CGEventSourceCreate(HID_SYSTEM_STATE);

            for down in [true, false] {
                let event = CGEventCreateKeyboardEvent(source, VK_F15, down);
                if event.is_null() {
                    if !source.is_null() {
                        CFRelease(source as CFTypeRef);
                    }
                    return Err(Error::Os {
                        call: "CGEventCreateKeyboardEvent",
                        code: 0,
                    });
                }
                CGEventPost(HID_EVENT_TAP, event);
                CFRelease(event as CFTypeRef);
            }

            if !source.is_null() {
                CFRelease(source as CFTypeRef);
            }
        }
        Ok(())
    }
}

/// Whether this process may post synthetic input.
pub fn is_trusted() -> bool {
    imp::is_trusted()
}

/// Trigger the OS Accessibility prompt. The answer appears in a later
/// [`is_trusted`] call, not in the return value.
pub fn request_trust() {
    imp::request_trust()
}

/// Idle time as presence detectors measure it.
pub fn idle() -> Duration {
    imp::idle()
}

pub fn screen_locked() -> bool {
    imp::screen_locked()
}

/// Post one harmless keystroke, resetting the idle counter.
///
/// Prefer [`Keeper::tick`], which applies the guard rails.
pub fn nudge() -> Result<()> {
    imp::post_nudge()
}

/// Applies the guard rails around [`nudge`].
#[derive(Debug, Clone)]
pub struct Keeper {
    pub threshold: Duration,
}

impl Default for Keeper {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_IDLE_THRESHOLD,
        }
    }
}

impl Keeper {
    /// Nudge if and only if it is currently appropriate.
    ///
    /// `Ok(true)` means a keystroke was posted, `Ok(false)` that conditions were not
    /// met — the ordinary case while the machine is in use.
    pub fn tick(&self) -> Result<bool> {
        if !should_nudge(is_trusted(), screen_locked(), idle(), self.threshold) {
            return Ok(false);
        }
        nudge()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nudges_only_when_idle_past_the_threshold() {
        let t = Duration::from_secs(120);
        assert!(!should_nudge(true, false, Duration::from_secs(119), t));
        assert!(should_nudge(true, false, Duration::from_secs(120), t));
        assert!(should_nudge(true, false, Duration::from_secs(600), t));
    }

    /// The point of the threshold: never land a synthetic keystroke in the middle of
    /// someone actually typing.
    #[test]
    fn never_nudges_while_the_machine_is_in_use() {
        let t = Duration::from_secs(120);
        assert!(!should_nudge(true, false, Duration::ZERO, t));
        assert!(!should_nudge(true, false, Duration::from_secs(1), t));
    }

    #[test]
    fn never_nudges_while_the_screen_is_locked() {
        let t = Duration::from_secs(120);
        assert!(!should_nudge(true, true, Duration::from_secs(600), t));
    }

    #[test]
    fn never_nudges_without_the_grant() {
        let t = Duration::from_secs(120);
        assert!(!should_nudge(false, false, Duration::from_secs(600), t));
    }

    /// `nudge` must report the missing grant rather than appear to succeed, because
    /// the underlying OS call gives no error of its own when untrusted.
    #[test]
    fn nudge_agrees_with_the_trust_check() {
        if !is_trusted() {
            assert!(nudge().is_err(), "must not claim success while untrusted");
        }
    }

    #[test]
    fn idle_is_a_plausible_reading() {
        let d = idle();
        assert!(
            d < Duration::from_secs(60 * 60 * 24 * 365),
            "implausible: {d:?}"
        );
    }

    #[test]
    fn screen_lock_query_does_not_panic() {
        let _ = screen_locked();
    }

    #[test]
    fn keeper_defaults_to_the_documented_threshold() {
        assert_eq!(Keeper::default().threshold, DEFAULT_IDLE_THRESHOLD);
    }
}
