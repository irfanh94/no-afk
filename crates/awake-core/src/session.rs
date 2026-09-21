//! Session lifecycle — durations, countdown, auto-end.
//!
//! Pure logic over an injectable [`Clock`], so the whole state machine is testable
//! without sleeping in tests or touching real power state.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{acquire, Backend, Flags, Guard, Request, Result};

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

pub trait Clock: Send + Sync + fmt::Debug {
    fn now(&self) -> Instant;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A clock that only moves when you tell it to.
#[derive(Debug)]
pub struct TestClock {
    base: Instant,
    offset: Mutex<Duration>,
}

impl Default for TestClock {
    fn default() -> Self {
        Self {
            base: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }
}

impl TestClock {
    pub fn advance(&self, by: Duration) {
        let mut off = self.offset.lock().unwrap_or_else(|e| e.into_inner());
        *off += by;
    }
}

impl Clock for TestClock {
    fn now(&self) -> Instant {
        let off = *self.offset.lock().unwrap_or_else(|e| e.into_inner());
        self.base + off
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Until the user turns it off, or the app quits.
    Indefinite,
    /// Auto-ends after this long.
    For(Duration),
}

#[derive(Debug, Clone)]
pub struct Session {
    pub kind: Kind,
    pub flags: Flags,
    pub reason: String,
    started: Instant,
}

impl Session {
    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
    }

    /// Time left, or `None` for an indefinite session. Saturates at zero.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        match self.kind {
            Kind::Indefinite => None,
            Kind::For(total) => Some(total.saturating_sub(self.elapsed(now))),
        }
    }

    pub fn is_expired(&self, now: Instant) -> bool {
        matches!(self.remaining(now), Some(Duration::ZERO))
    }
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

/// Owns at most one live session. This is the type the UI layer drives.
pub struct Manager {
    backend: Arc<dyn Backend>,
    clock: Arc<dyn Clock>,
    active: Option<(Session, Guard)>,
}

impl fmt::Debug for Manager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Manager")
            .field("backend", &self.backend.name())
            .field("active", &self.active.as_ref().map(|(s, _)| s))
            .finish()
    }
}

impl Manager {
    pub fn new(backend: Arc<dyn Backend>, clock: Arc<dyn Clock>) -> Self {
        Self {
            backend,
            clock,
            active: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub fn session(&self) -> Option<&Session> {
        self.active.as_ref().map(|(s, _)| s)
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.session()?.remaining(self.clock.now())
    }

    /// Start a session, replacing any existing one.
    ///
    /// The old session is released *before* the new one is acquired, so a failure to
    /// acquire leaves us cleanly off rather than holding stale assertions.
    pub fn start(&mut self, kind: Kind, flags: Flags, reason: impl Into<String>) -> Result<()> {
        self.stop()?;

        let reason = reason.into();
        let mut req = Request::new(flags, reason.clone());

        // Hand the deadline to the OS as well as tracking it ourselves. The kernel-side
        // timeout is what saves us if this process is suspended or SIGKILLed — our own
        // `tick()` can't run then.
        if let Kind::For(d) = kind {
            req = req.with_timeout(d);
        }

        let guard = acquire(Arc::clone(&self.backend), req)?;
        let session = Session {
            kind,
            flags,
            reason,
            started: self.clock.now(),
        };
        self.active = Some((session, guard));
        Ok(())
    }

    /// End the current session. No-op if there isn't one.
    pub fn stop(&mut self) -> Result<()> {
        match self.active.take() {
            Some((_, guard)) => guard.release(),
            None => Ok(()),
        }
    }

    /// Drive expiry. Call this roughly once a second from the UI.
    ///
    /// Returns `true` if a session just auto-ended, so the caller can update the tray
    /// and notify.
    pub fn tick(&mut self) -> Result<bool> {
        let expired = self
            .active
            .as_ref()
            .is_some_and(|(s, _)| s.is_expired(self.clock.now()));

        if expired {
            self.stop()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Reset the display idle timer. Belt-and-braces against the screensaver.
    ///
    /// Does **not** affect Slack/Teams presence: the HID idle counter they read is
    /// unaffected by this call.
    pub fn poke_display(&self) -> Result<()> {
        self.backend.declare_user_activity("session tick")
    }
}
