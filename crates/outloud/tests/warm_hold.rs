//! The warm hold must not become the always-open stream we removed.
//!
//! `crates/outloud/src/mic.rs` is explicit: the daemon used to hold the
//! input stream for the whole session, which lit the system recording
//! indicator all day while the tray said idle. That was replaced with
//! open-on-keydown / close-on-commit precisely so the orange dot means
//! "dictating right now", a claim the user can check.
//!
//! The warm hold trades a bounded piece of that, and only for devices
//! measured slower than the pre-roll window, because on those devices the
//! head of every utterance is lost and no downstream buffer can recover
//! audio the device never captured (docs/input-latency.md option 3 is
//! explicit that widening pre-roll does not help).
//!
//! These tests pin the boundaries of that trade.

use outloud::devlatency::{StartupWatch, PRE_ROLL_WINDOW};
use outloud::pipeline::Config;
use std::time::{Duration, Instant};

#[test]
fn the_warm_hold_is_off_by_default() {
    // The default must be the honest indicator, not the faster one. A
    // latency optimisation that silently keeps the microphone open is the
    // exact behaviour this product removed on purpose.
    assert_eq!(
        Config::default().warm_hold_ms,
        0,
        "warm hold must be opt-in"
    );
}

#[test]
fn a_fast_device_is_never_held_open() {
    // Holding a built-in microphone open buys nothing (it already starts
    // inside the pre-roll window) and costs the indicator. The gate is a
    // measurement, not a guess about the transport.
    let mut watch = StartupWatch::new();
    watch.on_device("MacBook Pro Microphone");
    let opened = Instant::now();
    watch.on_open(opened);
    // First sample comfortably inside the window.
    watch.on_first_audio(opened + PRE_ROLL_WINDOW / 2);
    assert!(
        !watch.current_device_is_slow(),
        "a device inside the pre-roll window must not qualify for a hold"
    );
}

#[test]
fn a_slow_device_qualifies_after_one_measurement() {
    // AirPods on this machine measured 210ms against a 150ms window.
    let mut watch = StartupWatch::new();
    watch.on_device("Jessie's AirPods");
    let opened = Instant::now();
    watch.on_open(opened);
    watch.on_first_audio(opened + Duration::from_millis(210));
    assert!(
        watch.current_device_is_slow(),
        "a device measured past the pre-roll window must qualify"
    );
}

#[test]
fn slowness_is_tracked_per_device_not_globally() {
    // Switching from a slow headset to the built-in microphone must not
    // leave the built-in microphone inheriting the hold.
    let mut watch = StartupWatch::new();
    watch.on_device("Slow Headset");
    let opened = Instant::now();
    watch.on_open(opened);
    watch.on_first_audio(opened + Duration::from_millis(300));
    assert!(watch.current_device_is_slow());

    watch.on_device("MacBook Pro Microphone");
    assert!(
        !watch.current_device_is_slow(),
        "slowness must not leak across a device change"
    );
}

#[test]
fn the_hold_is_bounded_by_the_schema() {
    // An unbounded hold is the always-open stream by another name. The
    // schema is what makes "bounded" checkable rather than asserted.
    let spec = config::schema::spec_for("microphone.warm-hold-ms").expect("the key must exist");
    assert!(
        spec.wired,
        "a documented-but-inert privacy knob is worse than none"
    );
    // The upper bound must be short enough that the indicator visibly
    // goes out between utterances rather than appearing to stay lit.
    assert!(
        spec.constraint.check(&config::Value::Int(10_000)).is_ok(),
        "10s must be within range"
    );
    assert!(
        spec.constraint.check(&config::Value::Int(60_000)).is_err(),
        "a minute-long hold is indistinguishable from always-open"
    );
    assert!(
        spec.constraint.check(&config::Value::Int(-1)).is_err(),
        "negative durations must be refused, not saturated"
    );
}

#[test]
fn adopting_a_warm_stream_does_not_unlearn_the_devices_slowness() {
    // The self-defeating loop this feature invites: while a warm hold is
    // running the stream is already flowing, so the gap from key-down to
    // the next chunk measures the poll interval, not the device. Recording
    // that would score the device fast, withdraw the hold, and restore the
    // clipping the hold was fixing -- with the measurement now "proving"
    // the device is fine.
    //
    // The pipeline avoids it by not stamping a new open when it adopts a
    // warm stream. This pins the property that makes that correct: a
    // measurement is only meaningful after a real open, so no stamp means
    // no verdict and the earlier judgement stands.
    let mut watch = StartupWatch::new();
    watch.on_device("Jessie's AirPods");
    let opened = Instant::now();
    watch.on_open(opened);
    watch.on_first_audio(opened + Duration::from_millis(210));
    assert!(watch.current_device_is_slow());

    // Second utterance adopts the warm stream: no on_open call at all.
    // The arriving chunk must not be scored against a stale or absent
    // open stamp.
    let verdict = watch.on_first_audio(Instant::now());
    assert_eq!(
        verdict,
        outloud::devlatency::Verdict::Fine,
        "an unstamped arrival must produce no verdict"
    );
    assert!(
        watch.current_device_is_slow(),
        "adopting a warm stream must not un-learn that the device is slow"
    );
}
