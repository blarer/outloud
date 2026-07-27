//! Pure event-matching: from (event type, keycode, flags) to a Down/Up edge
//! for one chord. Kept OS-independent and free of FFI so the trickiest part
//! of the tap backend, deciding what counts as our key going down or up, is
//! unit-tested on every platform with synthetic events.
//!
//! Three chord shapes need three different matching rules:
//!
//! - **Bare fn**: never a key event. It arrives as `flagsChanged` with
//!   keycode 63 and the NX_SECONDARYFNMASK bit indicating direction. The
//!   keycode filter is essential: arrow keys and F-keys also carry the fn
//!   flag bit in their events, and matching on the flag alone would fire the
//!   mic on every arrow press.
//! - **Bare side-specific modifier** (right-option): also `flagsChanged`,
//!   but direction comes from the *device-specific* NX bit (NX_DEVICERALTKEYMASK
//!   etc), not the generic one. The generic option bit stays set while
//!   EITHER option key is down, so releasing right option while left option
//!   is held would otherwise be missed and leave the mic hot.
//! - **Modifiers + key** (cmd+shift+space): `keyDown` where the keycode
//!   matches and the masked flags equal the chord exactly ("exactly" so that
//!   cmd+shift+space does not also fire on cmd+shift+alt+space). The
//!   matching `keyUp` is accepted on keycode alone, because users routinely
//!   release the modifiers before the key and requiring flags at key-up
//!   would lose the release, stranding push-to-talk in the hot state.

use crate::chord::{Chord, Key};
use crate::keycode::{self, MOD_COMPARE_MASK, MOD_FN};

/// CGEventType values we care about (CGEventTypes.h).
pub const EVENT_KEY_DOWN: u32 = 10;
pub const EVENT_KEY_UP: u32 = 11;
pub const EVENT_FLAGS_CHANGED: u32 = 12;
/// Sent INTO the callback when the tap was disabled for being too slow.
pub const EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFFFFFE;
/// Sent when something (e.g. secure input, another tool) disabled the tap.
pub const EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFFFFFF;

// NX device-specific modifier bits (IOKit hidsystem/IOLLEvent.h). These are
// the low bits of CGEventFlags and are the only way to tell left from right.
const DEVICE_L_CTRL: u64 = 0x0000_0001;
const DEVICE_L_SHIFT: u64 = 0x0000_0002;
const DEVICE_R_SHIFT: u64 = 0x0000_0004;
const DEVICE_L_CMD: u64 = 0x0000_0008;
const DEVICE_R_CMD: u64 = 0x0000_0010;
const DEVICE_L_ALT: u64 = 0x0000_0020;
const DEVICE_R_ALT: u64 = 0x0000_0040;
const DEVICE_R_CTRL: u64 = 0x0000_2000;

/// A key edge for OUR chord, as decided by the matcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Down,
    Up,
}

/// Compiled matching strategy for one chord.
#[derive(Debug, Clone, Copy)]
enum Strategy {
    BareFn,
    /// keycode of the physical modifier key + its device-specific flag bit.
    BareSideModifier {
        keycode: i64,
        device_bit: u64,
    },
    /// keycode + exact modifier flags required at key-down, compared under
    /// `mask`. The mask excludes the fn bit unless the chord asks for fn:
    /// F-keys, arrows, and the nav cluster intrinsically carry
    /// NX_SECONDARYFNMASK in their events (that is how the HID system marks
    /// "secondary function" keys), so requiring fn to be ABSENT would make
    /// every F-key chord unmatchable. Found the hard way by the demo
    /// selftest: F13 arrives with flags 0x800000 set.
    Keyed {
        keycode: i64,
        mods: u64,
        mask: u64,
    },
}

/// Stateful matcher: one per binding. State exists only to de-bounce
/// `flagsChanged` (which fires for EVERY modifier edge, not just ours) into
/// clean Down/Up pairs for the chord.
#[derive(Debug)]
pub struct Matcher {
    strategy: Strategy,
    /// For bare-modifier chords: whether we currently consider the key down,
    /// so repeated flagsChanged events for OTHER modifiers don't re-fire.
    down: bool,
}

/// Why a chord cannot be compiled into a matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatcherError {
    /// The chord's key has no known virtual keycode (exotic Char).
    UnmappableKey(String),
}

impl std::fmt::Display for MatcherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MatcherError::UnmappableKey(k) => {
                write!(f, "no virtual keycode known for key '{k}'")
            }
        }
    }
}

impl std::error::Error for MatcherError {}

impl Matcher {
    pub fn new(chord: &Chord) -> Result<Matcher, MatcherError> {
        let strategy = match chord.key {
            None => Strategy::BareFn,
            Some(k) if k.is_bare_modifier() => Strategy::BareSideModifier {
                keycode: keycode::keycode(chord).expect("bare modifiers all have keycodes"),
                device_bit: device_bit(k),
            },
            Some(_) => {
                let mods = keycode::mods_bits(chord);
                let mask = if mods & MOD_FN != 0 {
                    MOD_COMPARE_MASK
                } else {
                    MOD_COMPARE_MASK & !MOD_FN
                };
                Strategy::Keyed {
                    keycode: keycode::keycode(chord)
                        .ok_or_else(|| MatcherError::UnmappableKey(chord.to_string()))?,
                    mods,
                    mask,
                }
            }
        };
        Ok(Matcher {
            strategy,
            down: false,
        })
    }

    /// Feed one event; get the edge it represents for our chord, if any.
    /// Must stay allocation-free and cheap: the caller is the event-tap
    /// callback, which runs on the system's input-dispatch thread.
    pub fn feed(&mut self, event_type: u32, keycode: i64, flags: u64) -> Option<Edge> {
        match self.strategy {
            Strategy::BareFn => {
                if event_type != EVENT_FLAGS_CHANGED || keycode != keycode::KEY_FN {
                    return None;
                }
                self.edge_from_bit(flags & MOD_FN != 0)
            }
            Strategy::BareSideModifier {
                keycode: want,
                device_bit,
            } => {
                if event_type != EVENT_FLAGS_CHANGED || keycode != want {
                    return None;
                }
                self.edge_from_bit(flags & device_bit != 0)
            }
            Strategy::Keyed {
                keycode: want,
                mods,
                mask,
            } => {
                if keycode != want {
                    return None;
                }
                match event_type {
                    EVENT_KEY_DOWN if flags & mask == mods => self.edge_from_bit(true),
                    // Key-up matches on keycode alone; see module doc.
                    EVENT_KEY_UP => self.edge_from_bit(false),
                    _ => None,
                }
            }
        }
    }

    /// The tap died and was recreated; any missed key-up is gone. Callers
    /// pair this with `TapHold::reset()`.
    pub fn reset(&mut self) {
        self.down = false;
    }

    fn edge_from_bit(&mut self, now_down: bool) -> Option<Edge> {
        if now_down == self.down {
            // Auto-repeat (keyed) or an unrelated modifier edge (bare):
            // no state change, no event.
            return None;
        }
        self.down = now_down;
        Some(if now_down { Edge::Down } else { Edge::Up })
    }
}

fn device_bit(key: Key) -> u64 {
    match key {
        Key::LeftCommand => DEVICE_L_CMD,
        Key::RightCommand => DEVICE_R_CMD,
        Key::LeftOption => DEVICE_L_ALT,
        Key::RightOption => DEVICE_R_ALT,
        Key::LeftControl => DEVICE_L_CTRL,
        Key::RightControl => DEVICE_R_CTRL,
        Key::LeftShift => DEVICE_L_SHIFT,
        Key::RightShift => DEVICE_R_SHIFT,
        _ => unreachable!("only called for bare modifier keys"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keycode::{KEY_FN, KEY_RIGHT_OPTION, KEY_SPACE, MOD_COMMAND, MOD_OPTION, MOD_SHIFT};

    #[test]
    fn keyed_chord_down_up() {
        let chord: Chord = "cmd+shift+space".parse().unwrap();
        let mut m = Matcher::new(&chord).unwrap();
        assert_eq!(
            m.feed(EVENT_KEY_DOWN, KEY_SPACE, MOD_COMMAND | MOD_SHIFT),
            Some(Edge::Down)
        );
        // Release with mods already gone still counts (mods released first).
        assert_eq!(m.feed(EVENT_KEY_UP, KEY_SPACE, 0), Some(Edge::Up));
    }

    #[test]
    fn keyed_chord_requires_exact_mods_at_down() {
        let chord: Chord = "cmd+shift+space".parse().unwrap();
        let mut m = Matcher::new(&chord).unwrap();
        // Extra modifier: not our chord.
        assert_eq!(
            m.feed(
                EVENT_KEY_DOWN,
                KEY_SPACE,
                MOD_COMMAND | MOD_SHIFT | MOD_OPTION
            ),
            None
        );
        // Missing modifier: not our chord.
        assert_eq!(m.feed(EVENT_KEY_DOWN, KEY_SPACE, MOD_COMMAND), None);
        // Plain space while idle: keycode matches but nothing was down, and
        // the up edge is de-bounced away.
        assert_eq!(m.feed(EVENT_KEY_UP, KEY_SPACE, 0), None);
    }

    #[test]
    fn keyed_chord_ignores_caps_lock_noise() {
        let chord: Chord = "cmd+space".parse().unwrap();
        let mut m = Matcher::new(&chord).unwrap();
        const ALPHA_LOCK: u64 = 1 << 16;
        const NONCOALESCED: u64 = 1 << 8;
        assert_eq!(
            m.feed(
                EVENT_KEY_DOWN,
                KEY_SPACE,
                MOD_COMMAND | ALPHA_LOCK | NONCOALESCED
            ),
            Some(Edge::Down)
        );
    }

    #[test]
    fn f_key_chord_matches_despite_intrinsic_fn_flag() {
        // Real HID events for F13 carry NX_SECONDARYFNMASK (observed
        // flags=0x20800000 in the demo selftest). A bare "f13" binding must
        // still match.
        let chord: Chord = "f13".parse().unwrap();
        let mut m = Matcher::new(&chord).unwrap();
        assert_eq!(m.feed(EVENT_KEY_DOWN, 105, 0x2080_0000), Some(Edge::Down));
        assert_eq!(m.feed(EVENT_KEY_UP, 105, 0x2080_0000), Some(Edge::Up));
        // But an explicit modifier still gates: cmd+f13 must not fire bare.
        let chord: Chord = "cmd+f13".parse().unwrap();
        let mut m = Matcher::new(&chord).unwrap();
        assert_eq!(m.feed(EVENT_KEY_DOWN, 105, 0x2080_0000), None);
        assert_eq!(
            m.feed(EVENT_KEY_DOWN, 105, 0x2080_0000 | MOD_COMMAND),
            Some(Edge::Down)
        );
    }

    #[test]
    fn auto_repeat_downs_are_debounced() {
        let chord: Chord = "cmd+space".parse().unwrap();
        let mut m = Matcher::new(&chord).unwrap();
        assert_eq!(
            m.feed(EVENT_KEY_DOWN, KEY_SPACE, MOD_COMMAND),
            Some(Edge::Down)
        );
        assert_eq!(m.feed(EVENT_KEY_DOWN, KEY_SPACE, MOD_COMMAND), None);
        assert_eq!(m.feed(EVENT_KEY_DOWN, KEY_SPACE, MOD_COMMAND), None);
        assert_eq!(m.feed(EVENT_KEY_UP, KEY_SPACE, MOD_COMMAND), Some(Edge::Up));
    }

    #[test]
    fn bare_fn_needs_keycode_63() {
        let mut m = Matcher::new(&Chord::fn_key()).unwrap();
        // Arrow keys carry the fn flag but keycode != 63: must not fire.
        assert_eq!(
            m.feed(EVENT_FLAGS_CHANGED, 123, crate::keycode::MOD_FN),
            None
        );
        assert_eq!(m.feed(EVENT_KEY_DOWN, 123, crate::keycode::MOD_FN), None);
        // The real fn key.
        assert_eq!(
            m.feed(EVENT_FLAGS_CHANGED, KEY_FN, crate::keycode::MOD_FN),
            Some(Edge::Down)
        );
        assert_eq!(m.feed(EVENT_FLAGS_CHANGED, KEY_FN, 0), Some(Edge::Up));
    }

    #[test]
    fn right_option_release_seen_while_left_option_held() {
        let mut m = Matcher::new(&Chord::right_option()).unwrap();
        const DEV_R_ALT: u64 = 0x40;
        const DEV_L_ALT: u64 = 0x20;
        // Right option down (generic option bit + right device bit).
        assert_eq!(
            m.feed(
                EVENT_FLAGS_CHANGED,
                KEY_RIGHT_OPTION,
                MOD_OPTION | DEV_R_ALT
            ),
            Some(Edge::Down)
        );
        // Right option up while LEFT option still held: the generic option
        // bit is still set. The device bit is what must decide.
        assert_eq!(
            m.feed(
                EVENT_FLAGS_CHANGED,
                KEY_RIGHT_OPTION,
                MOD_OPTION | DEV_L_ALT
            ),
            Some(Edge::Up)
        );
    }

    #[test]
    fn left_option_edges_do_not_fire_right_option_binding() {
        let mut m = Matcher::new(&Chord::right_option()).unwrap();
        const DEV_L_ALT: u64 = 0x20;
        // flagsChanged for the LEFT option key has its own keycode (58).
        assert_eq!(
            m.feed(EVENT_FLAGS_CHANGED, 58, MOD_OPTION | DEV_L_ALT),
            None
        );
    }

    #[test]
    fn reset_clears_stuck_down() {
        let chord: Chord = "cmd+space".parse().unwrap();
        let mut m = Matcher::new(&chord).unwrap();
        m.feed(EVENT_KEY_DOWN, KEY_SPACE, MOD_COMMAND);
        m.reset();
        // After reset a fresh down fires again instead of being debounced.
        assert_eq!(
            m.feed(EVENT_KEY_DOWN, KEY_SPACE, MOD_COMMAND),
            Some(Edge::Down)
        );
    }
}
