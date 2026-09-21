//! An in-memory [`Backend`] for tests.
//!
//! Lets the session state machine be tested on any platform, and — more importantly —
//! lets tests assert that **nothing leaked**, which is the failure mode that actually
//! hurts users.

use std::collections::BTreeSet;
use std::sync::Mutex;

use crate::{Backend, Error, Flags, Handle, Request, Result, SystemAssertion};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Acquire { ids: Vec<u64>, flags: Flags, reason: String },
    Release { ids: Vec<u64> },
    UserActivity { reason: String },
}

#[derive(Debug, Default)]
struct State {
    next_id: u64,
    live: BTreeSet<u64>,
    events: Vec<Event>,
    fail_next_acquire: Option<Error>,
    fail_next_release: Option<Error>,
}

#[derive(Debug, Default)]
pub struct FakeBackend {
    state: Mutex<State>,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Assertions currently held. **Should be 0 at the end of every test.**
    pub fn live_count(&self) -> usize {
        self.lock().live.len()
    }

    pub fn live_ids(&self) -> Vec<u64> {
        self.lock().live.iter().copied().collect()
    }

    pub fn events(&self) -> Vec<Event> {
        self.lock().events.clone()
    }

    /// Make the next `acquire` fail, to exercise error paths.
    pub fn fail_next_acquire(&self, err: Error) {
        self.lock().fail_next_acquire = Some(err);
    }

    /// Make the next `release` fail — used to prove `Drop` does not panic.
    pub fn fail_next_release(&self, err: Error) {
        self.lock().fail_next_release = Some(err);
    }
}

impl Backend for FakeBackend {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn acquire(&self, req: &Request) -> Result<Handle> {
        let mut st = self.lock();
        if let Some(err) = st.fail_next_acquire.take() {
            return Err(err);
        }

        // Match the real macOS backend: one assertion per requested flag.
        let n = [req.flags.display, req.flags.system, req.flags.disk]
            .iter()
            .filter(|f| **f)
            .count();

        let mut ids = Vec::with_capacity(n);
        for _ in 0..n {
            st.next_id += 1;
            let id = st.next_id;
            st.live.insert(id);
            ids.push(id);
        }

        st.events.push(Event::Acquire {
            ids: ids.clone(),
            flags: req.flags,
            reason: req.reason.clone(),
        });
        Ok(Handle { ids, timed: req.timeout.is_some() })
    }

    fn release(&self, handle: &Handle) -> Result<()> {
        let mut st = self.lock();
        st.events.push(Event::Release { ids: handle.ids.clone() });

        if let Some(err) = st.fail_next_release.take() {
            return Err(err);
        }
        for id in &handle.ids {
            st.live.remove(id);
        }
        Ok(())
    }

    fn declare_user_activity(&self, reason: &str) -> Result<()> {
        self.lock().events.push(Event::UserActivity { reason: reason.to_string() });
        Ok(())
    }

    fn system_assertions(&self) -> Result<Vec<SystemAssertion>> {
        Ok(vec![SystemAssertion {
            pid: 1234,
            process: "FakeApp".into(),
            kind: "PreventUserIdleDisplaySleep".into(),
            name: "fake assertion".into(),
        }])
    }
}
