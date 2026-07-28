//! Pure Windows event-matching: from (virtual-key code, down/up) to a
//! Down/Up edge for one chord. The Windows sibling of [`crate::matcher`],
//! kept OS-independent and FFI-free for the same reason: the trickiest part
//! of the hook backend, deciding what counts as our key going down or up,
//! is unit-tested on every platform with synthetic events.
//!
//! Input model: the low-level keyboard hook (`WH_KEYBOARD_LL`) delivers a
//! `KBDLLHOOKSTRUCT` for EVERY key transition system-wide, with
//! side-specific virtual keys for modifiers (`VK_LMENU`/`VK_RMENU`, not the
//! generic `VK_MENU`). That makes the Windows problem *simpler* than macOS:
//! there is no `flagsChanged` reconstruction, a right-Alt press really is a
//! key-down of vk 0xA5. The matcher only has to
//!
//! - track which generic modifiers are currently held (from the
//!   side-specific VKs it sees), and
//! - decide whether a given transition is our chord's edge.
//!
//! The same asymmetric rule as macOS applies: key-DOWN requires the chord's
//! modifiers to match exactly (so ctrl+shift+space does not also fire on
//! ctrl+shift+alt+space), but key-UP is accepted on the key alone, because
//! users release modifiers before the key and losing the release would
//! leave push-to-talk hot, the worst failure available.

use crate::chord::{Chord, Key, Modifier};
use crate::matcher::{Edge, MatcherError};

// Virtual-key codes (winuser.h). Only the ones the matcher needs.
pub const VK_LSHIFT: u32 = 0xA0;
pub const VK_RSHIFT: u32 = 0xA1;
pub const VK_LCONTROL: u32 = 0xA2;
pub const VK_RCONTROL: u32 = 0xA3;
pub const VK_LMENU: u32 = 0xA4;
pub const VK_RMENU: u32 = 0xA5;
pub const VK_LWIN: u32 = 0x5B;
pub const VK_RWIN: u32 = 0x5C;
const VK_SPACE: u32 = 0x20;
const VK_TAB: u32 = 0x09;
const VK_ESCAPE: u32 = 0x1B;
const VK_RETURN: u32 = 0x0D;
const VK_F1: u32 = 0x70;

// Internal modifier bitmask, tracked from the side-specific VK stream.
const M_SHIFT: u8 = 1 << 0;
const M_CTRL: u8 = 1 << 1;
const M_ALT: u8 = 1 << 2;
const M_WIN: u8 = 1 << 3;

/// The virtual key for a chord's non-modifier key, if one exists.
///
/// Letters and digits map by ASCII identity (VK 'A'..'Z' and '0'..'9' are
/// their uppercase ASCII codes on every layout). Punctuation is DELIBERATELY
/// unmapped: the OEM VK codes (VK_OEM_1 etc) name physical positions whose
/// produced character depends on the active layout, so binding "ctrl+;"
/// through them would silently bind a different character on AZERTY. Refusing
/// with UnmappableKey is the loud failure; a config UI can offer layout-aware
/// capture later.
pub fn vk_for_key(key: Key) -> Option<u32> {
    match key {
        Key::Char(c) if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase() as u32),
        Key::Char(_) => None,
        Key::Space => Some(VK_SPACE),
        Key::Tab => Some(VK_TAB),
        Key::Escape => Some(VK_ESCAPE),
        Key::Return => Some(VK_RETURN),
        // VK_F1..VK_F24 are contiguous; the chord type allows F1..=F19.
        Key::F(n) => Some(VK_F1 + (n as u32) - 1),
        Key::LeftCommand => Some(VK_LWIN),
        Key::RightCommand => Some(VK_RWIN),
        Key::LeftOption => Some(VK_LMENU),
        Key::RightOption => Some(VK_RMENU),
        Key::LeftControl => Some(VK_LCONTROL),
        Key::RightControl => Some(VK_RCONTROL),
        Key::LeftShift => Some(VK_LSHIFT),
        Key::RightShift => Some(VK_RSHIFT),
    }
}

/// Which generic modifier a side-specific VK belongs to, for state tracking.
fn modifier_bit(vk: u32) -> Option<u8> {
    match vk {
        VK_LSHIFT | VK_RSHIFT => Some(M_SHIFT),
        VK_LCONTROL | VK_RCONTROL => Some(M_CTRL),
        VK_LMENU | VK_RMENU => Some(M_ALT),
        VK_LWIN | VK_RWIN => Some(M_WIN),
        _ => None,
    }
}

/// The modifier bitmask a chord requires. Mapping note: the chord vocabulary
/// is macOS-flavoured (cmd/alt/ctrl/shift); on Windows `cmd` means the
/// Windows key and `alt` means Alt, matching how cross-platform apps
/// (VS Code, Chromium) translate the same names.
fn required_mods(chord: &Chord) -> Result<u8, MatcherError> {
    let mut mods = 0u8;
    for m in &chord.mods {
        mods |= match m {
            Modifier::Shift => M_SHIFT,
            Modifier::Control => M_CTRL,
            Modifier::Option => M_ALT,
            Modifier::Command => M_WIN,
            // No Fn: the Fn key is handled by keyboard firmware on PCs and
            // never reaches the OS input stream, so a chord requiring it can
            // never match. Refusing at compile time beats a dead binding.
            Modifier::Fn => {
                return Err(MatcherError::UnmappableKey(
                    "fn is invisible to Windows (handled in keyboard firmware)".into(),
                ))
            }
        };
    }
    Ok(mods)
}

#[derive(Debug, Clone, Copy)]
enum Strategy {
    /// A bare side-specific modifier (right-alt PTT, the product default's
    /// nearest equivalent). Matches that VK's own transitions directly.
    BareModifier { vk: u32 },
    /// Modifiers + key.
    Keyed { vk: u32, mods: u8 },
}

/// Stateful matcher: one per binding, fed every hook event in order.
#[derive(Debug)]
pub struct WinMatcher {
    strategy: Strategy,
    /// Generic modifiers currently held, tracked from the event stream
    /// rather than polled with `GetAsyncKeyState`, so matching is a pure
    /// function of the events and therefore testable.
    mods_down: u8,
    /// Whether we consider our key currently down, to collapse the OS's
    /// typematic auto-repeat (repeated WM_KEYDOWN while held) into a single
    /// Down edge; forwarding repeats would re-trigger tap/hold constantly.
    down: bool,
}

impl WinMatcher {
    pub fn new(chord: &Chord) -> Result<WinMatcher, MatcherError> {
        let strategy = match chord.key {
            None => {
                // Bare fn is the only keyless chord shape, and fn does not
                // exist as an input on Windows (see required_mods).
                return Err(MatcherError::UnmappableKey(
                    "bare fn cannot be bound on Windows (handled in keyboard firmware)".into(),
                ));
            }
            Some(k) if k.is_bare_modifier() => Strategy::BareModifier {
                vk: vk_for_key(k).expect("bare modifiers all have VKs"),
            },
            Some(k) => Strategy::Keyed {
                vk: vk_for_key(k).ok_or_else(|| MatcherError::UnmappableKey(k_name(k)))?,
                mods: required_mods(chord)?,
            },
        };
        Ok(WinMatcher {
            strategy,
            mods_down: 0,
            down: false,
        })
    }

    /// Feed one hook event; get the edge it represents for our chord, if
    /// any. Must stay allocation-free and cheap: the caller is the
    /// WH_KEYBOARD_LL callback, which runs on the input-dispatch path and is
    /// silently unhooked by the OS if it is ever slow (docs/hotkeys.md).
    pub fn feed(&mut self, vk: u32, is_down: bool) -> Option<Edge> {
        // Track modifier state FIRST, so a chord whose key is itself a
        // modifier still sees consistent state.
        let mod_bit = modifier_bit(vk);

        let edge = match self.strategy {
            Strategy::BareModifier { vk: want } => {
                if vk != want {
                    None
                } else if is_down && !self.down {
                    self.down = true;
                    Some(Edge::Down)
                } else if !is_down && self.down {
                    self.down = false;
                    Some(Edge::Up)
                } else {
                    None
                }
            }
            Strategy::Keyed { vk: want, mods } => {
                if vk != want {
                    None
                } else if is_down && !self.down && self.mods_down == mods {
                    self.down = true;
                    Some(Edge::Down)
                } else if !is_down && self.down {
                    // Up on the key alone: modifiers may already be released.
                    self.down = false;
                    Some(Edge::Up)
                } else {
                    None
                }
            }
        };

        if let Some(bit) = mod_bit {
            if is_down {
                self.mods_down |= bit;
            } else {
                self.mods_down &= !bit;
            }
        }

        edge
    }

    /// Forget everything. Called when the hook may have missed events (the
    /// OS unhooked us, or the machine slept): stale "key is down" state
    /// keeps the mic hot forever, so pessimism is the safe direction.
    pub fn reset(&mut self) {
        self.mods_down = 0;
        self.down = false;
    }
}

fn k_name(k: Key) -> String {
    format!("{k:?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::Chord;

    fn chord(s: &str) -> Chord {
        s.parse().unwrap()
    }

    #[test]
    fn bare_right_alt_gives_clean_edges() {
        let mut m = WinMatcher::new(&chord("right-option")).unwrap();
        assert_eq!(m.feed(VK_RMENU, true), Some(Edge::Down));
        // Typematic repeat while held must not re-fire.
        assert_eq!(m.feed(VK_RMENU, true), None);
        assert_eq!(m.feed(VK_RMENU, false), Some(Edge::Up));
        // Left alt is a different physical key: silence.
        assert_eq!(m.feed(VK_LMENU, true), None);
        assert_eq!(m.feed(VK_LMENU, false), None);
    }

    #[test]
    fn keyed_chord_requires_exact_modifiers_at_down() {
        let mut m = WinMatcher::new(&chord("ctrl+shift+space")).unwrap();
        m.feed(VK_LCONTROL, true);
        assert_eq!(m.feed(VK_SPACE, true), None, "shift still missing");
        m.feed(VK_LSHIFT, true);
        assert_eq!(m.feed(VK_SPACE, false), None, "no down yet, no up");
        assert_eq!(m.feed(VK_SPACE, true), Some(Edge::Down));
        // Extra modifier while held: release still honoured.
        m.feed(VK_LMENU, true);
        assert_eq!(m.feed(VK_SPACE, false), Some(Edge::Up));
    }

    #[test]
    fn superset_of_modifiers_does_not_fire() {
        let mut m = WinMatcher::new(&chord("ctrl+shift+space")).unwrap();
        m.feed(VK_LCONTROL, true);
        m.feed(VK_LSHIFT, true);
        m.feed(VK_LMENU, true); // alt on top: not our chord
        assert_eq!(m.feed(VK_SPACE, true), None);
    }

    #[test]
    fn up_accepted_after_modifiers_released_first() {
        let mut m = WinMatcher::new(&chord("ctrl+space")).unwrap();
        m.feed(VK_RCONTROL, true);
        assert_eq!(m.feed(VK_SPACE, true), Some(Edge::Down));
        m.feed(VK_RCONTROL, false); // user lets go of ctrl first
        assert_eq!(m.feed(VK_SPACE, false), Some(Edge::Up));
    }

    #[test]
    fn either_side_satisfies_a_generic_modifier() {
        let mut m = WinMatcher::new(&chord("ctrl+space")).unwrap();
        m.feed(VK_RCONTROL, true);
        assert_eq!(m.feed(VK_SPACE, true), Some(Edge::Down));
        m.feed(VK_SPACE, false);
        m.feed(VK_RCONTROL, false);
        m.feed(VK_LCONTROL, true);
        assert_eq!(m.feed(VK_SPACE, true), Some(Edge::Down));
    }

    #[test]
    fn fn_chords_are_refused_loudly() {
        assert!(WinMatcher::new(&chord("fn")).is_err());
        assert!(WinMatcher::new(&chord("fn+f5")).is_err());
    }

    #[test]
    fn layout_dependent_punctuation_is_refused() {
        assert!(WinMatcher::new(&chord("ctrl+;")).is_err());
    }

    #[test]
    fn letters_digits_and_fkeys_map() {
        assert_eq!(vk_for_key(Key::Char('a')), Some('A' as u32));
        assert_eq!(vk_for_key(Key::Char('7')), Some('7' as u32));
        assert_eq!(vk_for_key(Key::F(13)), Some(0x7C));
    }

    #[test]
    fn reset_clears_stuck_state() {
        let mut m = WinMatcher::new(&chord("ctrl+space")).unwrap();
        m.feed(VK_LCONTROL, true);
        m.feed(VK_SPACE, true);
        m.reset();
        // After reset the key is not considered down, so an up is silent...
        assert_eq!(m.feed(VK_SPACE, false), None);
        // ...and ctrl must be pressed again for a new down.
        assert_eq!(m.feed(VK_SPACE, true), None);
    }
}

/// State-space tests for the hook-recovery contract.
///
/// The Windows hook can be removed by the OS mid-gesture with no
/// notification (docs/hotkeys.md). The watchdog's job is to make that
/// survivable, and the property that matters is not "we reinstalled" but
/// **the microphone never stays hot**. These exercise the exact
/// matcher+machine pair the hook callback drives, so the guarantee is
/// tested on every platform rather than argued in a comment.
#[cfg(test)]
mod recovery_tests {
    use super::*;
    use crate::chord::Chord;
    use crate::taphold::{HotkeyEvent, TapHold, Timing};
    use std::time::Instant;

    fn chord(s: &str) -> Chord {
        s.parse().unwrap()
    }

    /// The exact disaster the watchdog exists to prevent: the key goes
    /// down, the hook dies while it is held, and the key-UP is therefore
    /// never observed. Without a reset the machine stays in "capturing"
    /// forever, which means a live microphone with no way to stop it.
    #[test]
    fn recovery_closes_a_capture_whose_key_up_was_swallowed() {
        let mut m = WinMatcher::new(&chord("right-option")).unwrap();
        let mut machine = TapHold::new(Timing::default());
        let t0 = Instant::now();

        assert_eq!(m.feed(VK_RMENU, true), Some(Edge::Down));
        // Capture starts on the DOWN edge: push-to-talk must not wait for a
        // threshold, or the first word of every utterance is lost.
        assert_eq!(machine.on_key_down(t0), vec![HotkeyEvent::Pressed]);
        assert!(machine.capturing(), "the microphone is now live");

        // ---- the hook dies here; the key-up is never delivered ----
        // Without recovery the machine would sit in Pressed forever.

        // What the watchdog does on detection.
        m.reset();
        let recovered = machine.reset();

        assert!(
            !machine.capturing(),
            "the machine must not be left capturing after recovery: a stuck \
             capture is a permanently hot microphone"
        );
        assert_eq!(
            recovered,
            vec![HotkeyEvent::Released],
            "recovery must TELL the daemon capture ended, not just flip an \
             internal flag: the pipeline stops recording on Released"
        );

        // And the binding must be usable again immediately afterwards.
        assert_eq!(m.feed(VK_RMENU, true), Some(Edge::Down));
        assert_eq!(m.feed(VK_RMENU, false), Some(Edge::Up));
    }

    /// A reset while genuinely idle must not invent a Released event, or
    /// the daemon would try to commit an utterance nobody spoke.
    #[test]
    fn recovery_while_idle_emits_nothing() {
        let mut machine = TapHold::new(Timing::default());
        assert!(machine.reset().is_empty());
    }

    /// After recovery the matcher must not believe a key is still down:
    /// a stale "down" would swallow the user's next press (the edge would
    /// be suppressed as a typematic repeat) and the hotkey would look dead.
    #[test]
    fn recovery_does_not_swallow_the_next_press() {
        let mut m = WinMatcher::new(&chord("ctrl+space")).unwrap();
        m.feed(VK_LCONTROL, true);
        assert_eq!(m.feed(VK_SPACE, true), Some(Edge::Down));

        m.reset(); // hook died and was reinstalled

        // Modifiers must be re-pressed (we cannot know what is held), and
        // then the chord fires normally rather than being eaten.
        m.feed(VK_LCONTROL, true);
        assert_eq!(m.feed(VK_SPACE, true), Some(Edge::Down));
        assert_eq!(m.feed(VK_SPACE, false), Some(Edge::Up));
    }
}

/// Poison-recovery semantics for the Windows backend's global hook state.
///
/// The Windows backend keeps its matcher and state machine in a process
/// global `Mutex`, because a Win32 hook procedure is a bare function
/// pointer with no user-data argument (the macOS tap gets a `user_info`
/// pointer and needs no lock at all). That introduces a failure mode macOS
/// does not have: if any holder panics, the mutex is POISONED, and every
/// later `lock().unwrap()` panics too.
///
/// Why that matters here specifically: the watchdog's reset is what stops a
/// swallowed key-up from leaving the microphone hot. If poisoning made the
/// reset unreachable, a single unrelated panic would convert a recoverable
/// hook death into a permanently hot mic. So both non-callback lock sites
/// recover the guard instead of unwrapping. These tests pin the std
/// behaviour that argument rests on, on every platform.
#[cfg(test)]
mod poison_tests {
    use std::sync::Mutex;

    #[test]
    fn a_poisoned_mutex_still_yields_its_data() {
        let m: Mutex<u32> = Mutex::new(7);
        let _ = std::panic::catch_unwind(|| {
            let _g = m.lock().unwrap();
            panic!("holder died mid-update");
        });
        assert!(m.is_poisoned(), "the panic must have poisoned it");

        // The recovery the backend performs. Without into_inner, this line
        // would panic and the watchdog could never reset the state machine.
        let recovered = m.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(*recovered, 7, "the data survives poisoning intact");
    }

    #[test]
    fn recovery_is_repeatable_not_one_shot() {
        // Poison is sticky: every subsequent lock returns Err forever. The
        // backend must therefore recover at EVERY site, not once.
        let m: Mutex<u32> = Mutex::new(1);
        let _ = std::panic::catch_unwind(|| {
            let _g = m.lock().unwrap();
            panic!("boom");
        });
        for expected in [1, 2, 3] {
            let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
            assert_eq!(*g, expected);
            *g += 1;
        }
    }

    #[test]
    fn try_lock_on_a_poisoned_mutex_is_an_error_which_the_callback_treats_as_a_miss() {
        // The hook callback uses `if let Ok(..) = try_lock()`, so poisoning
        // makes it drop observations rather than panic. Dropping an event is
        // survivable (the watchdog resets); panicking across an
        // `extern "system"` boundary is undefined behaviour and is not.
        let m: Mutex<u32> = Mutex::new(0);
        let _ = std::panic::catch_unwind(|| {
            let _g = m.lock().unwrap();
            panic!("boom");
        });
        assert!(
            m.try_lock().is_err(),
            "a poisoned try_lock reports Err, which the callback skips safely"
        );
    }
}

/// Does poison CLEARING actually restore `try_lock`? The watchdog's
/// poison-recovery path depends on it: the hook callback acquires state
/// with `try_lock`, which returns `Err` forever on a poisoned mutex, so
/// without clearing, one panic anywhere would silently deafen the hotkey
/// while leaving the hook installed and the OS perfectly happy.
#[cfg(test)]
mod poison_clearing_tests {
    use std::sync::Mutex;

    fn poison(m: &Mutex<u32>) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = m.lock().unwrap();
            panic!("holder died");
        }));
    }

    #[test]
    fn clearing_poison_restores_try_lock() {
        let m: Mutex<u32> = Mutex::new(5);
        poison(&m);
        assert!(
            m.try_lock().is_err(),
            "before clearing, the callback's try_lock fails: every event dropped"
        );

        m.clear_poison();

        assert!(!m.is_poisoned());
        let g = m
            .try_lock()
            .expect("after clearing, the callback can take the state again");
        assert_eq!(*g, 5, "the data is intact across poison and recovery");
    }

    #[test]
    fn a_cleared_mutex_can_be_poisoned_and_cleared_again() {
        // The watchdog runs forever, so recovery must be repeatable rather
        // than a one-time rescue.
        let m: Mutex<u32> = Mutex::new(0);
        for _ in 0..3 {
            poison(&m);
            assert!(m.is_poisoned());
            m.clear_poison();
            assert!(m.try_lock().is_ok());
        }
    }
}
