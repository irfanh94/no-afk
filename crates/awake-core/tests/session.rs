//! Session + leak-safety tests, all against `FakeBackend` so they run on any platform.
//!
//! The recurring assertion in here is `live_count() == 0`. A leaked power assertion
//! means the user's machine silently never sleeps again, so every path that can end a
//! session gets checked for it.

use std::sync::Arc;
use std::time::Duration;

use awake_core::fake::{Event, FakeBackend};
use awake_core::session::{Clock, Kind, Manager, Session, SystemClock, TestClock};
use awake_core::{acquire, Error, Flags, Request};

fn manager() -> (Manager, Arc<FakeBackend>, Arc<TestClock>) {
    let backend = Arc::new(FakeBackend::new());
    let clock = Arc::new(TestClock::default());
    let mgr = Manager::new(backend.clone(), clock.clone());
    (mgr, backend, clock)
}

// ---------------------------------------------------------------------------
// Guard / leak safety
// ---------------------------------------------------------------------------

#[test]
fn guard_releases_on_drop() {
    let backend = Arc::new(FakeBackend::new());
    {
        let _guard = acquire(
            backend.clone(),
            Request::new(Flags::display_and_system(), "test"),
        )
        .unwrap();
        assert_eq!(backend.live_count(), 2, "display + system = two assertions");
    }
    assert_eq!(backend.live_count(), 0, "drop must release");
}

#[test]
fn guard_releases_when_unwinding_past_it() {
    let backend = Arc::new(FakeBackend::new());
    let b = backend.clone();

    let result = std::panic::catch_unwind(move || {
        let _guard = acquire(b, Request::new(Flags::display_and_system(), "test")).unwrap();
        panic!("boom");
    });

    assert!(result.is_err(), "the panic should have propagated");
    assert_eq!(backend.live_count(), 0, "a panic must not leak assertions");
}

#[test]
fn drop_does_not_panic_when_release_fails() {
    let backend = Arc::new(FakeBackend::new());
    backend.fail_next_release(Error::Os { call: "test", code: -1 });

    // Must not panic: a panic inside Drop during unwind aborts the process.
    drop(acquire(backend.clone(), Request::new(Flags::system_only(), "test")).unwrap());

    // The release was attempted and reported, even though it failed.
    assert!(matches!(backend.events().last(), Some(Event::Release { .. })));
}

#[test]
fn empty_request_is_rejected() {
    let backend = Arc::new(FakeBackend::new());
    let err = acquire(backend.clone(), Request::new(Flags::default(), "nothing")).unwrap_err();

    assert_eq!(err, Error::EmptyRequest);
    assert_eq!(backend.live_count(), 0);
}

#[test]
fn explicit_release_surfaces_errors_that_drop_would_swallow() {
    let backend = Arc::new(FakeBackend::new());
    backend.fail_next_release(Error::Os { call: "test", code: -7 });

    let guard = acquire(backend, Request::new(Flags::system_only(), "test")).unwrap();
    assert!(guard.release().is_err(), "release() should report what Drop hides");
}

// ---------------------------------------------------------------------------
// Session arithmetic
// ---------------------------------------------------------------------------

#[test]
fn indefinite_sessions_never_expire() {
    let (mut mgr, backend, clock) = manager();
    mgr.start(Kind::Indefinite, Flags::display_and_system(), "forever").unwrap();

    clock.advance(Duration::from_secs(60 * 60 * 24 * 7));

    assert_eq!(mgr.remaining(), None, "indefinite has no remaining time");
    assert!(!mgr.tick().unwrap(), "should not auto-end");
    assert!(mgr.is_active());
    assert_eq!(backend.live_count(), 2);
}

#[test]
fn timed_session_counts_down_and_auto_ends() {
    let (mut mgr, backend, clock) = manager();
    mgr.start(Kind::For(Duration::from_secs(600)), Flags::display_and_system(), "30m")
        .unwrap();

    assert_eq!(mgr.remaining(), Some(Duration::from_secs(600)));

    clock.advance(Duration::from_secs(240));
    assert_eq!(mgr.remaining(), Some(Duration::from_secs(360)));
    assert!(!mgr.tick().unwrap(), "not expired yet");
    assert_eq!(backend.live_count(), 2);

    clock.advance(Duration::from_secs(360));
    assert!(mgr.tick().unwrap(), "should report the auto-end");
    assert!(!mgr.is_active());
    assert_eq!(backend.live_count(), 0, "auto-end must release");
}

#[test]
fn remaining_saturates_at_zero_rather_than_underflowing() {
    let (mut mgr, _backend, clock) = manager();
    mgr.start(Kind::For(Duration::from_secs(10)), Flags::system_only(), "short").unwrap();

    clock.advance(Duration::from_secs(9999));

    assert_eq!(mgr.remaining(), Some(Duration::ZERO), "must not wrap around");
}

#[test]
fn tick_is_idempotent_after_expiry() {
    let (mut mgr, backend, clock) = manager();
    mgr.start(Kind::For(Duration::from_secs(5)), Flags::system_only(), "short").unwrap();
    clock.advance(Duration::from_secs(5));

    assert!(mgr.tick().unwrap(), "first tick reports the end");
    assert!(!mgr.tick().unwrap(), "second tick has nothing to report");
    assert!(!mgr.tick().unwrap());
    assert_eq!(backend.live_count(), 0);
}

// ---------------------------------------------------------------------------
// Manager lifecycle
// ---------------------------------------------------------------------------

#[test]
fn starting_a_new_session_replaces_the_old_one_without_leaking() {
    let (mut mgr, backend, _clock) = manager();

    mgr.start(Kind::Indefinite, Flags::display_and_system(), "first").unwrap();
    assert_eq!(backend.live_count(), 2);

    mgr.start(Kind::Indefinite, Flags::system_only(), "second").unwrap();
    assert_eq!(backend.live_count(), 1, "old session released, new one is system-only");
    assert_eq!(mgr.session().unwrap().reason, "second");
}

#[test]
fn a_failed_start_leaves_us_cleanly_off() {
    let (mut mgr, backend, _clock) = manager();
    mgr.start(Kind::Indefinite, Flags::display_and_system(), "first").unwrap();

    backend.fail_next_acquire(Error::Os { call: "test", code: -1 });
    assert!(mgr.start(Kind::Indefinite, Flags::system_only(), "doomed").is_err());

    assert!(!mgr.is_active(), "must not report an active session");
    assert_eq!(backend.live_count(), 0, "and must not hold stale assertions");
}

#[test]
fn stop_is_a_noop_when_idle() {
    let (mut mgr, backend, _clock) = manager();
    assert!(mgr.stop().is_ok());
    assert!(mgr.stop().is_ok());
    assert_eq!(backend.live_count(), 0);
}

#[test]
fn dropping_the_manager_releases_the_session() {
    let backend = Arc::new(FakeBackend::new());
    {
        let mut mgr = Manager::new(backend.clone(), Arc::new(TestClock::default()));
        mgr.start(Kind::Indefinite, Flags::display_and_system(), "test").unwrap();
        assert_eq!(backend.live_count(), 2);
    }
    assert_eq!(backend.live_count(), 0, "quitting the app must release");
}

// ---------------------------------------------------------------------------
// Request shape
// ---------------------------------------------------------------------------

#[test]
fn timed_sessions_hand_the_deadline_to_the_os_too() {
    // The kernel-side timeout is what protects users if this process is suspended or
    // SIGKILLed, so a timed session must always set it.
    let (mut mgr, _backend, _clock) = manager();
    mgr.start(Kind::For(Duration::from_secs(300)), Flags::system_only(), "5m").unwrap();

    let req = Request::new(Flags::system_only(), "5m").with_timeout(Duration::from_secs(300));
    assert_eq!(req.timeout, Some(Duration::from_secs(300)));
}

#[test]
fn reason_is_recorded_for_the_pmset_listing() {
    let (mut mgr, backend, _clock) = manager();
    mgr.start(Kind::Indefinite, Flags::display_and_system(), "manual toggle").unwrap();

    match backend.events().first() {
        Some(Event::Acquire { reason, flags, .. }) => {
            assert_eq!(reason, "manual toggle");
            assert_eq!(*flags, Flags::display_and_system());
        }
        other => panic!("expected an Acquire event, got {other:?}"),
    }
}

#[test]
fn system_clock_advances() {
    let a = SystemClock.now();
    std::thread::sleep(Duration::from_millis(5));
    assert!(SystemClock.now() > a);
}

#[test]
fn session_is_send_and_sync_friendly() {
    fn assert_send<T: Send>() {}
    assert_send::<Session>();
    assert_send::<Kind>();
}
