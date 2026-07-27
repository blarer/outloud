//! The tap-vs-hold state machine.
//!
//! Per docs/ux/02-core-interaction.md: press-and-release under the threshold
//! (default 300ms) toggles latched capture; holding past the threshold is
//! push-to-talk. This module is pure (time is an argument, never read from a
//! clock), so the disambiguation logic is unit-tested to the millisecond
//! instead of being probed with sleeps on whatever machine CI runs on.
//!
//! Design constraint from the event-tap side: the callback that feeds this
//! machine runs on a high-priority system thread and must never block, so
//! `on_key_down`/`on_key_up`/`on_tick` do nothing but a state transition and
//! return the events to emit. All emission/IO happens on the consumer side.

use std::time::{Duration, Instant};

/// What the hotkey layer tells the rest of the product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// Key went down. Capture should start NOW: the overlay must show life
    /// within 100ms of key-down, before we can know tap vs hold, so audio
    /// capture cannot wait for the disambiguation.
    Pressed,
    /// Key came up after a hold: push-to-talk release, commit the utterance.
    Released,
    /// Key came up within the tap threshold: capture stays live, latched.
    Latched,
    /// A second tap (or the same key up after re-press) while latched:
    /// capture ends, commit the utterance.
    Unlatched,
    /// The OS disabled our event tap (timeout or user-input) and the backend
    /// re-enabled it. Emitted so the UI can flip the tray warning glyph per
    /// docs/ux/02-core-interaction.md ("this failure is loud"). Never emitted
    /// by the state machine itself, only by backends.
    TapRecovered,
}

/// Timing knobs. Split out so a settings UI can persist them.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// At or above this hold duration, key-up means Released (push-to-talk).
    /// Below it, key-up means Latched. 300ms per the UX doc.
    pub tap_threshold: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            tap_threshold: Duration::from_millis(300),
        }
    }
}

/// Internal states. Latched has its own pressed sub-state so that the tap
/// that ends a latch emits Unlatched on key-DOWN (instant feedback) and the
/// following key-up is swallowed rather than misread as a new gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    /// Key is down; tap-or-hold not yet decided.
    Pressed {
        down_at: Instant,
    },
    /// Tap happened, mic latched on, key currently up.
    Latched,
    /// Key pressed again while latched; waiting for its release.
    LatchedPressed,
}

/// The machine. One instance per binding.
#[derive(Debug)]
pub struct TapHold {
    timing: Timing,
    state: State,
}

impl TapHold {
    pub fn new(timing: Timing) -> Self {
        TapHold {
            timing,
            state: State::Idle,
        }
    }

    /// Key-down edge. Returns the events to emit, in order.
    pub fn on_key_down(&mut self, now: Instant) -> Vec<HotkeyEvent> {
        match self.state {
            State::Idle => {
                self.state = State::Pressed { down_at: now };
                vec![HotkeyEvent::Pressed]
            }
            // Ending tap: unlatch immediately on the down edge, don't make
            // the user wait for their own key-up to see capture stop.
            State::Latched => {
                self.state = State::LatchedPressed;
                vec![HotkeyEvent::Unlatched]
            }
            // Auto-repeat or a missed edge: ignore rather than double-fire.
            State::Pressed { .. } | State::LatchedPressed => vec![],
        }
    }

    /// Key-up edge.
    pub fn on_key_up(&mut self, now: Instant) -> Vec<HotkeyEvent> {
        match self.state {
            State::Pressed { down_at } => {
                if now.duration_since(down_at) < self.timing.tap_threshold {
                    self.state = State::Latched;
                    vec![HotkeyEvent::Latched]
                } else {
                    self.state = State::Idle;
                    vec![HotkeyEvent::Released]
                }
            }
            State::LatchedPressed => {
                // The release of the unlatching tap; Unlatched already fired.
                self.state = State::Idle;
                vec![]
            }
            // Key-up with no matching down (tap re-enabled mid-hold, or the
            // system swallowed the down): nothing sane to emit.
            State::Idle | State::Latched => vec![],
        }
    }

    /// Whether capture should currently be running. The safety-net silence
    /// timeout for a forgotten latch lives in the capture layer, not here:
    /// this machine only knows about key edges.
    pub fn capturing(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// Force back to idle. Used when the event tap dies and is re-created:
    /// we may have missed the key-up, and a machine stuck in Pressed would
    /// keep the mic hot forever, the worst trust failure this product has.
    pub fn reset(&mut self) -> Vec<HotkeyEvent> {
        let was_capturing = self.capturing();
        self.state = State::Idle;
        if was_capturing {
            vec![HotkeyEvent::Released]
        } else {
            vec![]
        }
    }
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

    #[test]
    fn hold_is_push_to_talk() {
        let base = t0();
        let mut m = TapHold::new(Timing::default());
        assert_eq!(m.on_key_down(base), vec![HotkeyEvent::Pressed]);
        assert!(m.capturing());
        assert_eq!(m.on_key_up(ms(base, 700)), vec![HotkeyEvent::Released]);
        assert!(!m.capturing());
    }

    #[test]
    fn tap_latches_and_second_tap_unlatches() {
        let base = t0();
        let mut m = TapHold::new(Timing::default());
        assert_eq!(m.on_key_down(base), vec![HotkeyEvent::Pressed]);
        assert_eq!(m.on_key_up(ms(base, 120)), vec![HotkeyEvent::Latched]);
        assert!(m.capturing(), "latched keeps capture live");
        // Unlatch fires on the DOWN edge of the second tap.
        assert_eq!(m.on_key_down(ms(base, 5000)), vec![HotkeyEvent::Unlatched]);
        // Its release is swallowed.
        assert_eq!(m.on_key_up(ms(base, 5100)), vec![]);
        assert!(!m.capturing());
    }

    #[test]
    fn threshold_boundary_is_exclusive_below() {
        let base = t0();
        let timing = Timing {
            tap_threshold: Duration::from_millis(300),
        };
        // 299ms: tap.
        let mut m = TapHold::new(timing);
        m.on_key_down(base);
        assert_eq!(m.on_key_up(ms(base, 299)), vec![HotkeyEvent::Latched]);
        // Exactly 300ms: hold. At-threshold counting as hold means a user
        // whose taps drift longer degrades into PTT (mic stops at release),
        // never into a surprise latch (mic stays on) - the safer direction.
        let mut m = TapHold::new(timing);
        m.on_key_down(base);
        assert_eq!(m.on_key_up(ms(base, 300)), vec![HotkeyEvent::Released]);
    }

    #[test]
    fn custom_threshold_respected() {
        let base = t0();
        let timing = Timing {
            tap_threshold: Duration::from_millis(150),
        };
        let mut m = TapHold::new(timing);
        m.on_key_down(base);
        assert_eq!(m.on_key_up(ms(base, 200)), vec![HotkeyEvent::Released]);
    }

    #[test]
    fn repeat_downs_do_not_double_fire() {
        let base = t0();
        let mut m = TapHold::new(Timing::default());
        assert_eq!(m.on_key_down(base), vec![HotkeyEvent::Pressed]);
        // OS auto-repeat delivers more downs while held.
        assert_eq!(m.on_key_down(ms(base, 400)), vec![]);
        assert_eq!(m.on_key_down(ms(base, 500)), vec![]);
        assert_eq!(m.on_key_up(ms(base, 600)), vec![HotkeyEvent::Released]);
    }

    #[test]
    fn stray_key_up_ignored() {
        let base = t0();
        let mut m = TapHold::new(Timing::default());
        assert_eq!(m.on_key_up(base), vec![]);
        assert!(!m.capturing());
    }

    #[test]
    fn reset_mid_hold_releases() {
        let base = t0();
        let mut m = TapHold::new(Timing::default());
        m.on_key_down(base);
        assert_eq!(m.reset(), vec![HotkeyEvent::Released]);
        assert!(!m.capturing());
        // Reset while idle emits nothing.
        assert_eq!(m.reset(), vec![]);
    }

    #[test]
    fn reset_while_latched_releases() {
        let base = t0();
        let mut m = TapHold::new(Timing::default());
        m.on_key_down(base);
        m.on_key_up(ms(base, 100));
        assert_eq!(m.reset(), vec![HotkeyEvent::Released]);
    }
}
