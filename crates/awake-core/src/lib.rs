//! `awake-core` — cross-platform OS keep-awake primitives.
//!
//! No UI, no Tauri, no async runtime. Everything here is synchronous and
//! testable against [`fake::FakeBackend`].
//!
//! # The layer model
//!
//! Three independent OS subsystems, which most apps in this category conflate:
//!
//! - **L1** system sleep — [`Flags::system`]
//! - **L2** display sleep / screensaver / lock — [`Flags::display`]
//! - **L3** user-idle / presence (Slack "Away") — **not in this crate.** L3 cannot be
//!   done with power assertions at all. Slack and Teams read the HID idle counter
//!   directly, and no power assertion resets it (measured, not assumed), so L3 needs
//!   synthetic input injection and an Accessibility grant.
//!
//! On macOS, holding [`Flags::display`] suppresses the screensaver — and therefore the
//! lock screen — *including* an MDM-managed screensaver timeout. A managed profile only
//! sets the idle threshold; it does not add a second enforcement path, so no special
//! privileges are needed even on managed hardware.
//!
//! # Leak safety
//!
//! Measured on macOS 26: powerd releases a process's assertions when that process
//! dies. Two assertions held with a 24-hour timeout vanished immediately on
//! `SIGKILL`, so a crash cannot strand the machine awake — the OS cleans up. The
//! same holds on the other platforms by construction: a Windows power request is a
//! kernel object closed with the process, and a logind inhibitor is a file
//! descriptor closed with it.
//!
//! So the leak that actually matters is narrower than "the app died": it is losing
//! the [`Handle`] while still *running*, or staying alive but wedged. Hence:
//!
//! - [`Guard`] releases on [`Drop`], unconditionally — this is the one that counts,
//!   because a dropped-but-unreleased handle is unrecoverable while the process
//!   lives on holding it.
//! - [`Drop`] never panics; a failed release is logged, not propagated.
//! - Prefer [`Request::timeout`] over an app-side timer. The kernel owns the
//!   deadline, so it still fires if our process is `SIGSTOP`ped, deadlocked, or
//!   otherwise alive but no longer ticking — the cases process death does not cover.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

pub mod fake;
pub mod session;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(all(unix, not(target_os = "macos")))]
pub mod linux;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The OS call failed. `code` is the platform-native status
    /// (`IOReturn` on macOS, `GetLastError()` on Windows, errno/D-Bus on Linux).
    Os { call: &'static str, code: i64 },
    /// This platform (or this backend) does not implement the operation.
    Unsupported(&'static str),
    /// A request that asks for nothing. Almost always a caller bug, and silently
    /// acquiring nothing would look like a working keep-awake session.
    EmptyRequest,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Os { call, code } => write!(f, "{call} failed (code {code})"),
            Error::Unsupported(what) => write!(f, "unsupported on this platform: {what}"),
            Error::EmptyRequest => write!(f, "request asked for no assertions"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

/// Which OS idle behaviours to suppress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Flags {
    /// Prevent display sleep. On macOS this also suppresses the screensaver and
    /// therefore the lock screen.
    pub display: bool,
    /// Prevent idle *system* sleep. The display may still sleep — this is the
    /// "keep working with the lid open and the screen dark" case.
    pub system: bool,
    /// Keep disks spun up. Rarely wanted on SSD-only hardware.
    pub disk: bool,
}

impl Flags {
    /// The default user-facing "keep my Mac awake" behaviour: screen stays on,
    /// machine stays up. This is what CoffeeTea/Caffeine do.
    pub const fn display_and_system() -> Self {
        Self {
            display: true,
            system: true,
            disk: false,
        }
    }

    /// Stay awake but let the screen go dark — long downloads, builds, renders.
    pub const fn system_only() -> Self {
        Self {
            display: false,
            system: true,
            disk: false,
        }
    }

    pub const fn is_empty(self) -> bool {
        !self.display && !self.system && !self.disk
    }
}

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// A keep-awake request. Turn one into a live [`Guard`] with [`acquire`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub flags: Flags,
    /// Human-readable why. Surfaced to the user in `pmset -g assertions` /
    /// `powercfg /requests`, so make it legible: backends prefix it with `no-afk: `.
    pub reason: String,
    /// OS-enforced auto-release.
    ///
    /// Strongly preferred over an app-side timer: the kernel owns the deadline, so it
    /// still fires if our process is alive but no longer ticking — suspended,
    /// deadlocked, or `SIGSTOP`ped. (Process *death* needs no help; the OS releases
    /// a dead process's assertions itself.)
    pub timeout: Option<Duration>,
}

impl Request {
    pub fn new(flags: Flags, reason: impl Into<String>) -> Self {
        Self {
            flags,
            reason: reason.into(),
            timeout: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// Opaque per-platform assertion handle.
///
/// This is a *list* because macOS allows exactly one type per assertion, so
/// `display + system` is two live assertions, not one. Windows and Linux collapse to
/// one entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Handle {
    pub ids: Vec<u64>,
    /// These assertions carry an OS-enforced timeout, so the kernel may already have
    /// released them behind our back.
    ///
    /// This exists to keep error handling honest. On macOS, "you released an id I
    /// don't know" and "that assertion already expired" are the *same* return code
    /// (see `macos::IO_RETURN_BAD_ARGUMENT`), so a backend cannot tell a benign
    /// double-release from a caller passing garbage. Rather than swallow the code
    /// unconditionally — which would hide real bugs — backends tolerate it only when
    /// this flag says an expiry was actually possible.
    pub timed: bool,
}

/// One entry from the OS-wide assertion list — the data behind the
/// "why is my Mac awake?" panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemAssertion {
    pub pid: i32,
    pub process: String,
    pub kind: String,
    pub name: String,
}

/// A platform implementation. Swap in [`fake::FakeBackend`] to test session logic
/// without touching real OS power state.
pub trait Backend: Send + Sync + fmt::Debug {
    fn name(&self) -> &'static str;

    fn acquire(&self, req: &Request) -> Result<Handle>;

    /// Release everything in `handle`. Must be idempotent-tolerant and must attempt
    /// *every* id even if an earlier one fails — a partial release is a leak.
    fn release(&self, handle: &Handle) -> Result<()>;

    /// Reset the OS display-idle timer once, waking the display if it is asleep.
    ///
    /// Note: this does **not** reset the HID idle counter that Slack/Teams read — that
    /// was measured, and the counter climbs straight through this call. It is
    /// belt-and-braces for the screensaver, not a presence feature.
    fn declare_user_activity(&self, _reason: &str) -> Result<()> {
        Err(Error::Unsupported("declare_user_activity"))
    }

    /// Every assertion currently held on the system, by any process.
    fn system_assertions(&self) -> Result<Vec<SystemAssertion>> {
        Err(Error::Unsupported("system_assertions"))
    }
}

/// The backend for the current platform.
pub fn default_backend() -> Arc<dyn Backend> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::IoKitBackend::default())
    }
    #[cfg(target_os = "windows")]
    {
        Arc::new(windows::PowerRequestBackend::default())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Arc::new(linux::DbusBackend::default())
    }
}

// ---------------------------------------------------------------------------
// Guard
// ---------------------------------------------------------------------------

/// A live keep-awake session. Releases on drop.
///
/// Held assertions are released when this value goes out of scope, panics unwind past
/// it, or the process exits normally. `SIGKILL` needs no cover here — the OS releases
/// a dead process's assertions itself. What neither this nor process death covers is a
/// process still alive but no longer ticking; that is what [`Request::timeout`] is for.
pub struct Guard {
    backend: Arc<dyn Backend>,
    handle: Handle,
    flags: Flags,
    released: bool,
}

impl Guard {
    pub fn flags(&self) -> Flags {
        self.flags
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    /// Release explicitly, surfacing any error. [`Drop`] does this too, but swallows
    /// failures — use this when the caller can actually report a problem.
    pub fn release(mut self) -> Result<()> {
        self.released = true;
        self.backend.release(&self.handle)
    }
}

impl fmt::Debug for Guard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Guard")
            .field("backend", &self.backend.name())
            .field("flags", &self.flags)
            .field("handle", &self.handle)
            .finish()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        // Never panic in Drop, and never swallow silently: a failed release leaves the
        // user's machine permanently awake with no UI affordance to fix it.
        if let Err(err) = self.backend.release(&self.handle) {
            eprintln!(
                "no-afk: FAILED to release power assertion {:?} via {}: {err}. \
                 The system may not sleep until this process exits.",
                self.handle,
                self.backend.name()
            );
        }
    }
}

/// Acquire a keep-awake session.
pub fn acquire(backend: Arc<dyn Backend>, req: Request) -> Result<Guard> {
    if req.flags.is_empty() {
        return Err(Error::EmptyRequest);
    }
    let handle = backend.acquire(&req)?;
    Ok(Guard {
        backend,
        handle,
        flags: req.flags,
        released: false,
    })
}
