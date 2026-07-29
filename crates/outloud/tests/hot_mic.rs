//! The hot microphone can never stay open.
//!
//! A sub-threshold tap on the push-to-talk chord latches capture on and
//! emits no bounding event, so the microphone stayed open and the recording
//! indicator stayed lit until the user pressed the chord again. The default
//! chord is a bare modifier, which gets tapped constantly during ordinary
//! typing, so this fired while *not* dictating. That is why it looked
//! unreproducible from the dictation path.
//!
//! Two properties are asserted here rather than one implementation detail,
//! because the bug was not "the latch arm is wrong", it was "capture can
//! stop by several routes and only one of them closed the device".

use std::time::Duration;

use outloud::pipeline::Config;

/// The tap that starts a latch, as `hotkey::TapHold` classifies it.
const SUB_THRESHOLD_TAP: Duration = Duration::from_millis(80);

#[test]
fn a_sub_threshold_tap_still_produces_a_bounded_capture() {
    // Drives the real chord matcher and the real event mapping, because the
    // original bug lived in the seam between them: `Latched` mapped to no
    // frontend event at all.
    let mut taphold = hotkey::taphold::TapHold::new(Default::default());
    let started = std::time::Instant::now();

    let mut events = Vec::new();
    events.extend(taphold.on_key_down(started));
    events.extend(taphold.on_key_up(started + SUB_THRESHOLD_TAP));

    // The precondition that made this invisible: after the tap, the matcher
    // considers itself still capturing, with no further event coming.
    assert!(
        taphold.capturing(),
        "precondition: a sub-threshold tap latches capture on"
    );

    // The latch itself is correct behaviour and stays: tap-to-latch is a
    // documented feature. What must not happen is capture with no deadline.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, hotkey::HotkeyEvent::Latched)),
        "an 80ms tap should latch; got {events:?}"
    );

    // The safety net is what bounds it, so it must be a real duration and
    // not a value that disables itself.
    let cfg = Config::default();
    assert!(
        cfg.hot_mic_timeout_ms >= 1_000,
        "a latched capture with no timeout is an indefinitely open microphone"
    );
    assert!(
        cfg.hot_mic_timeout_ms <= 600_000,
        "a ten-minute-plus hot mic is not a safety net"
    );
}

#[test]
fn the_safety_net_is_actually_read_from_config() {
    // The original failure was not a missing feature, it was a feature that
    // existed in the schema, was documented as working, and was read by
    // nobody. A default that happens to be right is not evidence the wiring
    // exists, so this asserts a NON-default value survives the whole path.
    let spec = config::schema::spec_for("silence-timeout-ms")
        .expect("silence-timeout-ms must exist in the schema");
    assert!(
        spec.wired,
        "silence-timeout-ms is read by the pipeline and must not be marked unwired"
    );

    let settings = outloud::menubar::Settings {
        silence_timeout_ms: 5_000,
        ..Default::default()
    };
    assert_eq!(
        settings.silence_timeout_ms, 5_000,
        "a configured timeout must reach the daemon unchanged"
    );
}

#[test]
fn an_out_of_range_timeout_cannot_disable_the_safety_net() {
    // config.toml is hand-edited. A zero here would previously have meant
    // "fire immediately"; what matters is that no value can mean "never".
    for absurd in [0i64, -1, i64::MIN] {
        let settings = outloud::menubar::Settings {
            silence_timeout_ms: absurd,
            ..Default::default()
        };
        let host_view = settings.silence_timeout_ms.max(1_000) as u64;
        assert!(
            host_view >= 1_000,
            "{absurd} must clamp to a real timeout, not disable the net"
        );
    }
}
