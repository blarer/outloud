//! macOS virtual keycodes and modifier masks, shared by the event-tap
//! backend (matching incoming events against a chord) and conflict detection
//! (comparing a chord against com.apple.symbolichotkeys entries, which store
//! the same keycodes and the same NX modifier bit layout).
//!
//! Kept in one place because a chord matched with one table and
//! conflict-checked with another would report "no conflict" for a chord that
//! then never fires: the exact silent-dead-hotkey outcome this crate exists
//! to prevent.

use crate::chord::{Chord, Key, Modifier};

// Carbon virtual keycodes (HIToolbox/Events.h). Stable since classic Mac OS;
// hardcoding beats pulling in a Carbon dependency for constants.
pub const KEY_RETURN: i64 = 36;
pub const KEY_TAB: i64 = 48;
pub const KEY_SPACE: i64 = 49;
pub const KEY_ESCAPE: i64 = 53;
pub const KEY_LEFT_COMMAND: i64 = 55;
pub const KEY_RIGHT_COMMAND: i64 = 54;
pub const KEY_LEFT_SHIFT: i64 = 56;
pub const KEY_RIGHT_SHIFT: i64 = 60;
pub const KEY_LEFT_OPTION: i64 = 58;
pub const KEY_RIGHT_OPTION: i64 = 61;
pub const KEY_LEFT_CONTROL: i64 = 59;
pub const KEY_RIGHT_CONTROL: i64 = 62;
pub const KEY_FN: i64 = 63;

// NX device-independent modifier masks, as used both by CGEventFlags and by
// the third parameter of symbolichotkeys entries.
pub const MOD_SHIFT: u64 = 1 << 17; // 131072
pub const MOD_CONTROL: u64 = 1 << 18; // 262144
pub const MOD_OPTION: u64 = 1 << 19; // 524288
pub const MOD_COMMAND: u64 = 1 << 20; // 1048576
pub const MOD_FN: u64 = 1 << 23; // 8388608, NX_SECONDARYFNMASK

/// All modifier bits we compare on. CGEventFlags carries extra noise
/// (alpha-lock, per-side bits, NX_NONCOALESCED); masking to this set before
/// comparison is what makes chord matching immune to caps-lock being on.
pub const MOD_COMPARE_MASK: u64 = MOD_SHIFT | MOD_CONTROL | MOD_OPTION | MOD_COMMAND | MOD_FN;

/// Chord modifiers as an NX bitmask.
pub fn mods_bits(chord: &Chord) -> u64 {
    let mut bits = 0u64;
    for m in &chord.mods {
        bits |= match m {
            Modifier::Shift => MOD_SHIFT,
            Modifier::Control => MOD_CONTROL,
            Modifier::Option => MOD_OPTION,
            Modifier::Command => MOD_COMMAND,
            Modifier::Fn => MOD_FN,
        };
    }
    bits
}

/// The virtual keycode a chord's key lands on with a US ANSI layout, or None
/// for chords with no key (bare fn).
///
/// LAYOUT CAVEAT: for `Key::Char` this table is ANSI-US. Keycodes are
/// positional, so on AZERTY the physical key at "a"'s ANSI position types
/// "q". Good enough for conflict *warnings* (symbolichotkeys stores the same
/// positional codes) and for the default bindings, which avoid character
/// keys entirely. A layout-aware picker (press-to-set, per the UX doc)
/// bypasses this table because it captures the keycode directly.
pub fn keycode(chord: &Chord) -> Option<i64> {
    let key = chord.key?;
    let code = match key {
        Key::Space => KEY_SPACE,
        Key::Tab => KEY_TAB,
        Key::Escape => KEY_ESCAPE,
        Key::Return => KEY_RETURN,
        Key::LeftCommand => KEY_LEFT_COMMAND,
        Key::RightCommand => KEY_RIGHT_COMMAND,
        Key::LeftOption => KEY_LEFT_OPTION,
        Key::RightOption => KEY_RIGHT_OPTION,
        Key::LeftControl => KEY_LEFT_CONTROL,
        Key::RightControl => KEY_RIGHT_CONTROL,
        Key::LeftShift => KEY_LEFT_SHIFT,
        Key::RightShift => KEY_RIGHT_SHIFT,
        Key::F(n) => f_keycode(n)?,
        Key::Char(c) => ansi_char_keycode(c)?,
    };
    Some(code)
}

fn f_keycode(n: u8) -> Option<i64> {
    // The F-row codes are historical and non-contiguous.
    Some(match n {
        1 => 122,
        2 => 120,
        3 => 99,
        4 => 118,
        5 => 96,
        6 => 97,
        7 => 98,
        8 => 100,
        9 => 101,
        10 => 109,
        11 => 103,
        12 => 111,
        13 => 105,
        14 => 107,
        15 => 113,
        16 => 106,
        17 => 64,
        18 => 79,
        19 => 80,
        _ => return None,
    })
}

fn ansi_char_keycode(c: char) -> Option<i64> {
    Some(match c.to_ascii_lowercase() {
        'a' => 0,
        's' => 1,
        'd' => 2,
        'f' => 3,
        'h' => 4,
        'g' => 5,
        'z' => 6,
        'x' => 7,
        'c' => 8,
        'v' => 9,
        'b' => 11,
        'q' => 12,
        'w' => 13,
        'e' => 14,
        'r' => 15,
        'y' => 16,
        't' => 17,
        '1' => 18,
        '2' => 19,
        '3' => 20,
        '4' => 21,
        '6' => 22,
        '5' => 23,
        '=' => 24,
        '9' => 25,
        '7' => 26,
        '-' => 27,
        '8' => 28,
        '0' => 29,
        ']' => 30,
        'o' => 31,
        'u' => 32,
        '[' => 33,
        'i' => 34,
        'p' => 35,
        'l' => 37,
        'j' => 38,
        '\'' => 39,
        'k' => 40,
        ';' => 41,
        '\\' => 42,
        ',' => 43,
        '/' => 44,
        'n' => 45,
        'm' => 46,
        '.' => 47,
        '`' => 50,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_shift_space_maps() {
        let c: Chord = "cmd+shift+space".parse().unwrap();
        assert_eq!(keycode(&c), Some(KEY_SPACE));
        assert_eq!(mods_bits(&c), MOD_COMMAND | MOD_SHIFT);
    }

    #[test]
    fn bare_fn_has_no_keycode_but_fn_bit() {
        let c: Chord = "fn".parse().unwrap();
        assert_eq!(keycode(&c), None);
        assert_eq!(mods_bits(&c), MOD_FN);
    }

    #[test]
    fn side_specific_modifiers_have_keycodes() {
        assert_eq!(
            keycode(&"right-option".parse().unwrap()),
            Some(KEY_RIGHT_OPTION)
        );
        assert_eq!(
            keycode(&"left-cmd".parse().unwrap()),
            Some(KEY_LEFT_COMMAND)
        );
    }
}
