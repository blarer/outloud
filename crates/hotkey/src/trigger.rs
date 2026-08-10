//! Pure trigger-event handling for the Linux compositor-exec backend
//! (`backend::linux`), kept out of that module and un-cfg-gated so it
//! compiles and is unit-tested on every platform, including this
//! development machine (macOS), which cannot run the Linux backend itself.
//!
//! ## Why a separate module
//!
//! `backend::linux` is gated `cfg(all(unix, not(target_os = "macos")))`
//! because it opens a real unix-domain socket. That gate means its own
//! test module never compiles on a Mac, so anything living only there is
//! unverifiable here. Everything that is actually *logic* rather than IO
//! (deciding what a PRESS/RELEASE line does to the tap-hold state machine,
//! deciding whether the watchdog should fire) is factored out to this
//! module instead, which has no cfg gate at all and is driven by synthetic
//! events exactly the way `matcher.rs` and `taphold.rs` already are.
//!
//! ## What this does NOT need to debounce
//!
//! The macOS/Windows backends run a stateful `Matcher` in front of
//! `TapHold` specifically to collapse a noisy hardware event stream (OS
//! auto-repeat, `flagsChanged` firing for every modifier in the system)
//! into clean edges. The Linux trigger transport has no such noise: the
//! compositor already decided which physical key matters and only execs
//! our CLI on the two edges it cares about. So a PRESS message maps
//! straight to `TapHold::on_key_down`, and `TapHold` ITSELF is already
//! idempotent against duplicates (see its `Pressed { .. } => vec![]` and
//! `Idle => vec![]` arms) so a doubled PRESS (a retried exec, a stray
//! second bind firing) or a doubled RELEASE (release arriving twice) is
//! absorbed for free, with no separate debounce layer required here.

use std::time::{Duration, Instant};

use crate::taphold::{HotkeyEvent, TapHold};

/// One line of the trigger protocol, from the compositor's exec (via the
/// `outloud trigger <verb>` CLI) or from a liveness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerVerb {
    /// The bound key went down. Hyprland's plain `bind`.
    Press,
    /// The bound key came up. Hyprland's `bindr` (release variant).
    Release,
    /// Not a key edge: "is a daemon listening at all", used by the doctor
    /// and by a human running the CLI by hand to sanity-check the socket
    /// without accidentally starting a capture. Must never touch the state
    /// machine, or a health check would itself trigger a dictation.
    Ping,
}

/// Why a trigger line could not be understood. Carries the raw token so the
/// caller's `ERR` response can quote exactly what arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadVerb(pub String);

impl std::fmt::Display for BadVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown trigger verb '{}' (want PRESS, RELEASE, or PING)",
            self.0
        )
    }
}

impl std::error::Error for BadVerb {}

impl TriggerVerb {
    /// Parse one protocol line (already trimmed of its newline). Case
    /// matters: the wire format is fixed and generated only by our own CLI
    /// and doctor, so there is no user-facing casing convention to be
    /// lenient about, and being strict makes a typo in a hand-rolled
    /// compositor exec fail loudly instead of silently doing nothing.
    pub fn parse(line: &str) -> Result<TriggerVerb, BadVerb> {
        match line.trim() {
            "PRESS" => Ok(TriggerVerb::Press),
            "RELEASE" => Ok(TriggerVerb::Release),
            "PING" => Ok(TriggerVerb::Ping),
            other => Err(BadVerb(other.to_string())),
        }
    }

    /// The wire form, without a trailing newline; the caller appends one.
    pub fn as_line(self) -> &'static str {
        match self {
            TriggerVerb::Press => "PRESS",
            TriggerVerb::Release => "RELEASE",
            TriggerVerb::Ping => "PING",
        }
    }
}

/// Feed one trigger event into the tap-hold machine, returning the events to
/// emit. Pure and side-effect-free (time is an argument, matching every
/// other consumer of `TapHold`), so it is testable with synthetic verbs
/// exactly like `matcher.rs` is testable with synthetic CGEvents.
///
/// `Ping` deliberately touches nothing: it exists so a liveness probe can
/// prove the daemon is listening without moving the state machine, which
/// matters because the doctor and a curious human both want to run it while
/// dictation is not supposed to start.
pub fn apply(machine: &mut TapHold, verb: TriggerVerb, now: Instant) -> Vec<HotkeyEvent> {
    match verb {
        TriggerVerb::Press => machine.on_key_down(now),
        TriggerVerb::Release => machine.on_key_up(now),
        TriggerVerb::Ping => vec![],
    }
}

/// How long a PRESS may go without a matching RELEASE before the backend
/// assumes the RELEASE message was lost and forces the machine back to
/// idle.
///
/// ## Why this exists
///
/// The macOS and Windows backends both carry a recovery path for a key-up
/// that the OS itself failed to deliver (a disabled event tap, an unhooked
/// low-level hook): without it, a state machine stuck in `Pressed` keeps
/// the microphone open forever, which both those modules call the worst
/// trust failure this crate can produce. The trigger-IPC transport has the
/// exact same failure shape with a different cause: the compositor's
/// `bindr` exec can fail independently of the matching `bind` exec (a
/// crashed helper, a `$PATH` that resolves differently for the release
/// keybind, a compositor reload that drops one binding but not the other),
/// and unlike an OS event tap there is no "tap disabled" notification to
/// react to -- the daemon has no signal at all that a release was supposed
/// to arrive and did not.
///
/// ## Why 120 seconds and not something tighter
///
/// The macOS/Windows watchdogs poll every 2 seconds because they are
/// reacting to an OS-level condition (hook removed, tap disabled) that is
/// itself detectable independent of any particular utterance's length; a
/// short poll there costs nothing and buys fast recovery. This watchdog has
/// no independent signal: the ONLY evidence available is "how long has the
/// key been down", so the timeout must be longer than any real,
/// intentional hold, or a person who pauses mid-thought gets cut off
/// wrongly, which trades a real failure (mic stuck forever) for a fake one
/// (utterance truncated) at the wrong exchange rate. 120s is well past any
/// believable single dictation (the pipeline's own `hot_mic_timeout_ms`
/// safety net, `crates/outloud/src/pipeline.rs`, already closes the
/// microphone at 60s by default for exactly this reason) while still being
/// a genuine backstop rather than a theoretical one: a truly lost RELEASE
/// message recovers within two minutes instead of holding the microphone
/// open until the process is restarted.
///
/// Overridable via `OUTLOUD_HOTKEY_TRIGGER_WATCHDOG_MS` (read in
/// `backend::linux`, not here, to keep this module free of environment
/// reads and therefore trivially testable).
pub const DEFAULT_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(120);

/// Whether the watchdog should force a reset: a PRESS is outstanding and
/// has been outstanding for at least `timeout`.
///
/// Pure predicate, split out from the loop that calls it so the boundary
/// condition is exercised precisely rather than by sleeping in a test.
pub fn watchdog_expired(pressed_at: Instant, now: Instant, timeout: Duration) -> bool {
    now.duration_since(pressed_at) >= timeout
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn ms(base: Instant, m: u64) -> Instant {
        base + Duration::from_millis(m)
    }

    // -- verb parsing ---------------------------------------------------

    #[test]
    fn parses_known_verbs() {
        assert_eq!(TriggerVerb::parse("PRESS"), Ok(TriggerVerb::Press));
        assert_eq!(TriggerVerb::parse("RELEASE"), Ok(TriggerVerb::Release));
        assert_eq!(TriggerVerb::parse("PING"), Ok(TriggerVerb::Ping));
    }

    #[test]
    fn trims_surrounding_whitespace_and_newline() {
        assert_eq!(TriggerVerb::parse("PRESS\n"), Ok(TriggerVerb::Press));
        assert_eq!(TriggerVerb::parse("  RELEASE  "), Ok(TriggerVerb::Release));
    }

    #[test]
    fn rejects_unknown_verbs_and_names_the_bad_token() {
        let err = TriggerVerb::parse("press").unwrap_err(); // case-sensitive
        assert_eq!(err.0, "press");
        assert!(err.to_string().contains("press"));
        assert!(TriggerVerb::parse("").is_err());
        assert!(TriggerVerb::parse("PRESS RELEASE").is_err());
    }

    #[test]
    fn verb_roundtrips_through_as_line() {
        for v in [TriggerVerb::Press, TriggerVerb::Release, TriggerVerb::Ping] {
            assert_eq!(TriggerVerb::parse(v.as_line()), Ok(v));
        }
    }

    // -- apply(): the same tap-hold machine, driven by trigger verbs ----

    #[test]
    fn hold_is_push_to_talk_via_trigger_events() {
        let base = t0();
        let mut m = TapHold::new(crate::taphold::Timing::default());
        assert_eq!(
            apply(&mut m, TriggerVerb::Press, base),
            vec![HotkeyEvent::Pressed]
        );
        assert!(m.capturing());
        assert_eq!(
            apply(&mut m, TriggerVerb::Release, ms(base, 700)),
            vec![HotkeyEvent::Released]
        );
        assert!(!m.capturing());
    }

    #[test]
    fn tap_latches_via_trigger_events() {
        let base = t0();
        let mut m = TapHold::new(crate::taphold::Timing::default());
        assert_eq!(
            apply(&mut m, TriggerVerb::Press, base),
            vec![HotkeyEvent::Pressed]
        );
        assert_eq!(
            apply(&mut m, TriggerVerb::Release, ms(base, 120)),
            vec![HotkeyEvent::Latched]
        );
        assert!(m.capturing());
        assert_eq!(
            apply(&mut m, TriggerVerb::Press, ms(base, 4000)),
            vec![HotkeyEvent::Unlatched]
        );
        // Unlatched fires on the DOWN edge (instant feedback); the matching
        // RELEASE of that same unlatching tap still has to arrive before
        // the machine is back to Idle, exactly like the other backends.
        assert_eq!(apply(&mut m, TriggerVerb::Release, ms(base, 4100)), vec![]);
        assert!(!m.capturing());
    }

    #[test]
    fn duplicate_press_is_absorbed_by_the_state_machine_itself() {
        // A compositor firing `bind` twice for one physical press (or our
        // CLI being invoked twice by an over-eager keybind) must not
        // double-start capture: TapHold's own Pressed arm swallows repeats.
        let base = t0();
        let mut m = TapHold::new(crate::taphold::Timing::default());
        assert_eq!(
            apply(&mut m, TriggerVerb::Press, base),
            vec![HotkeyEvent::Pressed]
        );
        assert_eq!(apply(&mut m, TriggerVerb::Press, ms(base, 10)), vec![]);
        assert_eq!(apply(&mut m, TriggerVerb::Press, ms(base, 20)), vec![]);
        assert!(m.capturing(), "still one live capture, not stopped");
    }

    #[test]
    fn duplicate_release_while_idle_is_a_silent_noop() {
        // A stray RELEASE with no matching PRESS (compositor race, a `bindr`
        // firing after the watchdog already reset us) must not panic or
        // emit a spurious commit.
        let base = t0();
        let mut m = TapHold::new(crate::taphold::Timing::default());
        assert_eq!(apply(&mut m, TriggerVerb::Release, base), vec![]);
        assert!(!m.capturing());
    }

    #[test]
    fn ping_never_touches_the_machine() {
        let base = t0();
        let mut m = TapHold::new(crate::taphold::Timing::default());
        assert_eq!(apply(&mut m, TriggerVerb::Ping, base), vec![]);
        assert!(!m.capturing());
        // And PING mid-capture does not end it either.
        apply(&mut m, TriggerVerb::Press, base);
        assert_eq!(apply(&mut m, TriggerVerb::Ping, ms(base, 50)), vec![]);
        assert!(m.capturing(), "PING must be a pure liveness probe");
    }

    // -- watchdog ---------------------------------------------------------

    #[test]
    fn watchdog_does_not_fire_before_the_deadline() {
        let base = t0();
        let timeout = Duration::from_secs(120);
        assert!(!watchdog_expired(base, ms(base, 119_999), timeout));
    }

    #[test]
    fn watchdog_fires_at_and_past_the_deadline() {
        let base = t0();
        let timeout = Duration::from_secs(120);
        assert!(watchdog_expired(base, base + timeout, timeout));
        assert!(watchdog_expired(
            base,
            base + timeout + Duration::from_secs(1),
            timeout
        ));
    }

    #[test]
    fn a_lost_release_recovers_through_reset_like_the_other_backends() {
        // What backend::linux actually does on watchdog expiry: reset the
        // machine, which is the exact recovery path macOS/Windows use when
        // THEY detect a swallowed key-up. Asserted here on the pure
        // machine so the contract is pinned independent of the socket IO.
        let base = t0();
        let mut m = TapHold::new(crate::taphold::Timing::default());
        apply(&mut m, TriggerVerb::Press, base);
        assert!(m.capturing());
        assert!(watchdog_expired(
            base,
            ms(base, 120_000),
            DEFAULT_WATCHDOG_TIMEOUT
        ));
        assert_eq!(m.reset(), vec![HotkeyEvent::Released]);
        assert!(
            !m.capturing(),
            "watchdog recovery must not leave the mic hot"
        );
    }
}
