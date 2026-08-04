//! Test-only helpers for process-global delivery state.
//!
//! `OUTLOUD_NO_INJECT` is an environment variable, so it is shared by every
//! test in the binary, and Rust runs tests in parallel threads. A test that
//! set it directly would silently turn a CONCURRENT test's `deliver` call
//! into `Outcome::Suppressed`. That was not hypothetical: three tests set
//! this variable, and the collision reproduced 32 times in 40 runs.
//!
//! The lock and the variable are bound together here so they cannot drift
//! apart: you cannot set the switch without holding the lock, and tests
//! that merely READ it (any test calling `deliver`) take the same lock.

use std::sync::{Mutex, MutexGuard};

/// Serialises everything that depends on delivery suppression.
static DELIVER_ENV: Mutex<()> = Mutex::new(());

// Set only by `allow_inject`, for the few tests that want a real write.
//
// Delivery is suppressed by DEFAULT in test builds (see
// `delivery_suppressed_by_default`). Two pipeline tests ran the real path
// and pasted their fixture sentence into the developer's clipboard,
// destroying whatever was there. Fixing those two by hand does not hold:
// the next test to drive the supervisor loop reintroduces it silently, and
// the suite stays green because the damage is outside everything it
// asserts on.
//
// So the default is inverted. Writing to the user's machine should be the
// loud, deliberate case, not the one you get by forgetting.
//
// THREAD-local, not global. Tests run in parallel threads, so a global
// opt-in leaks into whatever else is running: the first version of this
// made an unrelated test observe "delivery permitted" and fail on roughly
// every run at --test-threads=16. Scoping it to the thread makes one
// test's choice unobservable to every other test by construction, rather
// than by a lock everyone has to remember to take.
thread_local! {
    static ALLOW_INJECT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a test build should suppress delivery without being asked.
///
/// Compile-time rather than a `ctor` or a convention, so it applies to
/// every test that exists now or later, with no dependency and nothing to
/// remember.
pub fn delivery_suppressed_by_default() -> bool {
    !ALLOW_INJECT.with(|a| a.get())
}

/// Exclude concurrent changes to the suppression switch.
///
/// Taken by tests that call `deliver` and expect a REAL outcome. Poisoning
/// is ignored deliberately: it means an unrelated test panicked while
/// holding the lock, and cascading that into every other test replaces one
/// clear failure with a dozen confusing ones.
pub fn deliver_lock() -> MutexGuard<'static, ()> {
    DELIVER_ENV.lock().unwrap_or_else(|p| p.into_inner())
}

/// Turn delivery suppression on for as long as the returned guard lives.
///
/// Redundant with the test-build default, and kept because it also holds
/// the lock: a test that asserts on `Outcome::Suppressed` needs to know no
/// sibling is mid-`allow_inject`.
#[must_use = "delivery is only suppressed while the guard is alive"]
pub fn no_inject() -> NoInject {
    let guard = deliver_lock();
    // SAFETY: the lock makes this the only thread touching the environment,
    // and Drop clears the variable before releasing it.
    unsafe { std::env::set_var("OUTLOUD_NO_INJECT", "1") };
    NoInject { _guard: guard }
}

/// Clears `OUTLOUD_NO_INJECT` and releases the lock, in that order.
pub struct NoInject {
    _guard: MutexGuard<'static, ()>,
}

impl Drop for NoInject {
    fn drop(&mut self) {
        // SAFETY: still holding the lock; the guard drops after this.
        unsafe { std::env::remove_var("OUTLOUD_NO_INJECT") };
    }
}

/// Permit a REAL delivery for the duration of the returned guard.
///
/// The opt-in half of the inverted default. Holds the same lock, so it
/// cannot race a test that expects suppression, and restores the default on
/// drop even if the test panics.
///
/// Think hard before using this: it types into whatever the developer
/// running the suite has focused, and puts the fixture text on their
/// clipboard.
#[must_use = "delivery is only permitted while the guard is alive"]
pub fn allow_inject() -> AllowInject {
    let guard = deliver_lock();
    ALLOW_INJECT.with(|a| a.set(true));
    AllowInject { _guard: guard }
}

/// Restores the suppressed default when dropped.
pub struct AllowInject {
    _guard: MutexGuard<'static, ()>,
}

impl Drop for AllowInject {
    fn drop(&mut self) {
        ALLOW_INJECT.with(|a| a.set(false));
    }
}
