//! Windows backend — **not yet implemented**.
//!
//! Implementation notes, so this is a fill-in-the-blanks job rather than a research
//! job:
//!
//! **Primary: `PowerCreateRequest` / `PowerSetRequest` / `PowerClearRequest`**
//! (`powrprof.dll`, Windows 8+).
//!
//! ```text
//! REASON_CONTEXT ctx { Version: POWER_REQUEST_CONTEXT_VERSION,
//!                      Flags:   POWER_REQUEST_CONTEXT_SIMPLE_STRING,
//!                      Reason:  L"no-afk: <reason>" };
//! HANDLE h = PowerCreateRequest(&ctx);
//! PowerSetRequest(h, PowerRequestDisplayRequired);    // Flags::display
//! PowerSetRequest(h, PowerRequestSystemRequired);     // Flags::system
//! PowerSetRequest(h, PowerRequestExecutionRequired);  // Modern Standby (S0)
//! ```
//!
//! Store the `HANDLE` in [`Handle::ids`] as a `u64`. Release = `PowerClearRequest` for
//! each type set, then `CloseHandle`. Requests show up in `powercfg /requests`, the
//! Windows analogue of `pmset -g assertions` — name them legibly for the same reason.
//!
//! **Fallback: `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED |
//! ES_DISPLAY_REQUIRED)`.** Simpler but materially weaker: it is *per-thread* (the
//! calling thread must stay alive for the whole session), it only resets the idle
//! timer, and it never sets `PowerRequestExecutionRequired`, so it does not keep work
//! running across Modern Standby. Clear with `ES_CONTINUOUS` alone.
//!
//! **No OS-side timeout exists.** Unlike macOS's `kIOPMAssertionTimeoutKey`, Windows
//! has no kernel-enforced deadline, so [`Request::timeout`] must be honoured by an
//! app-side timer here. That is a real robustness regression versus macOS — a wedged
//! process keeps the machine awake indefinitely. Consider a watchdog.
//!
//! **Lock screen:** `PowerRequestDisplayRequired` blocks the display-timeout path to
//! lock. A GPO `InactivityTimeoutSecs` forced lock cannot be blocked this way — that is
//! L3 (`SendInput`) and out of scope for this crate.

use crate::{Backend, Error, Handle, Request, Result};

#[derive(Debug, Default)]
pub struct PowerRequestBackend;

impl Backend for PowerRequestBackend {
    fn name(&self) -> &'static str {
        "windows/powerrequest"
    }

    fn acquire(&self, _req: &Request) -> Result<Handle> {
        Err(Error::Unsupported("keep-awake is not implemented on Windows yet"))
    }

    fn release(&self, _handle: &Handle) -> Result<()> {
        Err(Error::Unsupported("keep-awake is not implemented on Windows yet"))
    }
}
