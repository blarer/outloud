//! Test-only helpers for process-global state.
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

/// Serialises everything that depends on `OUTLOUD_NO_INJECT`.
static DELIVER_ENV: Mutex<()> = Mutex::new(());

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
