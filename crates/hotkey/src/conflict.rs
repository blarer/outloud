//! Conflict detection: is this chord already claimed before we bind it?
//!
//! A silently dead hotkey is the worst outcome this crate can produce (the
//! UX doc is explicit: "Never silently accept a dead key"), and the
//! CGEventTap backend makes it easy to produce one: a listen-only tap SEES
//! every event even when Spotlight also handles it, so the bug is not "we
//! never fire", it is "we fire AND Spotlight opens", or for consumed chords
//! (input-source switch) the system acts and we double-act. Detection is
//! therefore advisory-by-severity, surfaced before binding, not a runtime
//! failure.
//!
//! Sources checked, in order of reliability:
//! 1. `com.apple.symbolichotkeys` preference domain: every system shortcut
//!    (Spotlight, Mission Control, input sources, screenshots...) with its
//!    keycode, modifier bits, and enabled flag. Read via `defaults export`
//!    (XML plist) and parsed here; parsing is a pure function of the XML so
//!    it is unit-tested against captured fixtures rather than the machine's
//!    live settings.
//! 2. A short static table of chords macOS claims OUTSIDE symbolichotkeys
//!    (cmd+tab app switcher, the Globe/fn press-to-act mappings from
//!    com.apple.HIToolbox), which never appear in the plist.
//!
//! What we cannot see: chords registered by other running apps via Carbon
//! RegisterEventHotKey. There is a private CopySymbolicHotKeys but no public
//! enumeration API. Honest answer per docs/hotkeys.md: we warn about what is
//! knowable and re-check liveness at runtime instead of pretending this list
//! is complete.

use std::collections::BTreeMap;
use std::fmt;

use crate::chord::Chord;
use crate::keycode::{self, MOD_COMPARE_MASK};

/// How bad a collision is. Advisory: the caller may still bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The system will also act on this chord (it is enabled). Binding it
    /// gives the user two effects per press.
    Claimed,
    /// The chord belongs to a known system function that is currently
    /// disabled. Fine today; flips back the moment the user re-enables it.
    ClaimedButDisabled,
}

/// One detected collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub severity: Severity,
    /// Human-readable owner, e.g. "Spotlight search" or "app switcher".
    pub owner: String,
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.severity {
            Severity::Claimed => write!(f, "already used by {} (enabled)", self.owner),
            Severity::ClaimedButDisabled => {
                write!(f, "assigned to {} (currently disabled)", self.owner)
            }
        }
    }
}

/// A system shortcut as recorded in com.apple.symbolichotkeys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolicHotkey {
    /// The AppleSymbolicHotKeys dictionary key ("64" is Spotlight, etc).
    pub id: u32,
    pub enabled: bool,
    /// Virtual keycode (parameters[1]). 65535 means "no key assigned".
    pub keycode: i64,
    /// NX modifier bitmask (parameters[2]).
    pub modifiers: u64,
}

/// Check a chord against a set of parsed symbolic hotkeys plus the static
/// always-claimed table. Pure so tests can feed synthetic tables.
pub fn find_conflicts(chord: &Chord, system: &[SymbolicHotkey]) -> Vec<Conflict> {
    let mut out = Vec::new();
    let want_mods = keycode::mods_bits(chord);
    let want_key = keycode::keycode(chord);

    // Bare fn: symbolichotkeys cannot express it, but com.apple.HIToolbox's
    // AppleFnUsageType can claim the *press* of the key (Change Input
    // Source / Show Emoji / Start Dictation). We flag it statically; the
    // live value is read by the platform probe in read_system_hotkeys().
    if chord.is_bare_modifier() && want_key.is_none() {
        out.push(Conflict {
            severity: Severity::Claimed,
            owner: "the system's Globe-key action (System Settings > Keyboard > \
                    'Press Globe key to...'); set it to 'Do Nothing' or presses will \
                    also trigger it"
                .to_string(),
        });
        return out;
    }

    let Some(want_key) = want_key else {
        return out;
    };

    for hk in system {
        // 65535 (0xFFFF) is the plist's "unassigned" sentinel; matching it
        // would flag every keyless entry against every chord.
        if hk.keycode == 65535 || hk.keycode != want_key {
            continue;
        }
        if hk.modifiers & MOD_COMPARE_MASK != want_mods {
            continue;
        }
        out.push(Conflict {
            severity: if hk.enabled {
                Severity::Claimed
            } else {
                Severity::ClaimedButDisabled
            },
            owner: symbolic_hotkey_name(hk.id),
        });
    }

    // System chords that never appear in symbolichotkeys.
    for (name, key, mods) in STATIC_SYSTEM_CHORDS {
        if *key == want_key && *mods == want_mods {
            out.push(Conflict {
                severity: Severity::Claimed,
                owner: (*name).to_string(),
            });
        }
    }

    out
}

/// Chords the OS claims outside the symbolichotkeys domain. Small on
/// purpose: entries earn their place by being unconditionally active.
const STATIC_SYSTEM_CHORDS: &[(&str, i64, u64)] = &[
    (
        "the app switcher (cmd+tab)",
        keycode::KEY_TAB,
        keycode::MOD_COMMAND,
    ),
    (
        "app window cycling (cmd+`)",
        50, // ANSI backtick
        keycode::MOD_COMMAND,
    ),
    ("force quit (cmd+alt+escape)", keycode::KEY_ESCAPE, {
        keycode::MOD_COMMAND | keycode::MOD_OPTION
    }),
];

/// Human names for the AppleSymbolicHotKeys ids a user is likely to collide
/// with. The full table is ~200 entries and undocumented; unknown ids fall
/// back to a numbered description, which still tells the user WHERE to look.
pub fn symbolic_hotkey_name(id: u32) -> String {
    let known: &[(u32, &str)] = &[
        (32, "Mission Control"),
        (33, "Application Windows"),
        (36, "Show Desktop"),
        (60, "Select previous input source (was ctrl+space default)"),
        (61, "Select next input source"),
        (64, "Spotlight search"),
        (65, "Spotlight Finder search window"),
        (79, "Move left a Space"),
        (81, "Move right a Space"),
        (160, "Launchpad"),
        (184, "Screenshot to file (shift+cmd+3)"),
        (28, "Screenshot to file (shift+cmd+3)"),
        (29, "Screenshot to clipboard"),
        (30, "Screenshot of area to file (shift+cmd+4)"),
        (31, "Screenshot of area to clipboard"),
        (222, "Notification Center"),
    ];
    known
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, n)| (*n).to_string())
        .unwrap_or_else(|| {
            format!("a system shortcut (AppleSymbolicHotKeys id {id}, see System Settings > Keyboard > Keyboard Shortcuts)")
        })
}

/// Parse the XML plist produced by
/// `defaults export com.apple.symbolichotkeys -`.
///
/// WHY a hand parser and not a plist crate: the structure we need is one
/// fixed shape (dict of dicts with an `enabled` bool and a three-integer
/// `parameters` array), the file is machine-generated (no exotic escaping in
/// the fields we touch), and this workspace treats every new dependency as a
/// supply-chain liability (see deny.toml). The parser is deliberately
/// lenient: an entry it cannot understand is SKIPPED, not fatal, because
/// failing conflict detection entirely over one malformed entry would kill
/// the more important warnings.
pub fn parse_symbolichotkeys_xml(xml: &str) -> Vec<SymbolicHotkey> {
    let mut out = Vec::new();
    // Find the AppleSymbolicHotKeys outer dict, then walk `<key>N</key>`
    // entries. Each entry's value dict spans to the matching </dict>.
    let Some(start) = xml.find("<key>AppleSymbolicHotKeys</key>") else {
        return out;
    };
    let body = &xml[start..];
    let mut entries: BTreeMap<u32, &str> = BTreeMap::new();
    let mut rest = body;
    while let Some(kpos) = rest.find("<key>") {
        let after = &rest[kpos + 5..];
        let Some(kend) = after.find("</key>") else {
            break;
        };
        let key_text = &after[..kend];
        rest = &after[kend..];
        // Only numeric keys are hotkey ids; "enabled"/"value"/"parameters"
        // keys inside entries fail the parse and are skipped naturally.
        let Ok(id) = key_text.trim().parse::<u32>() else {
            continue;
        };
        let Some(dict_start) = rest.find("<dict>") else {
            continue;
        };
        let Some(dict_len) = matching_dict_len(&rest[dict_start..]) else {
            continue;
        };
        entries.insert(id, &rest[dict_start..dict_start + dict_len]);
    }

    for (id, entry) in entries {
        // `enabled` is the first <true/>/<false/> after the enabled key.
        let enabled = entry
            .find("<key>enabled</key>")
            .map(|p| {
                let after = &entry[p..];
                match (after.find("<true/>"), after.find("<false/>")) {
                    (Some(t), Some(f)) => t < f,
                    (Some(_), None) => true,
                    _ => false,
                }
            })
            .unwrap_or(false);
        let ints = parameter_integers(entry);
        // parameters = [character, keycode, modifiers]. Entries without all
        // three (some are type "button") cannot collide with a keyboard
        // chord and are skipped.
        if ints.len() < 3 {
            continue;
        }
        out.push(SymbolicHotkey {
            id,
            enabled,
            keycode: ints[1],
            modifiers: ints[2] as u64,
        });
    }
    out
}

/// Length of the balanced `<dict>...</dict>` starting at the beginning of
/// `s` (which must start with `<dict>`), or None if unbalanced.
fn matching_dict_len(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = 0usize;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if s[i..].starts_with("<dict>") {
            depth += 1;
            i += 6;
        } else if s[i..].starts_with("</dict>") {
            depth -= 1;
            i += 7;
            if depth == 0 {
                return Some(i);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// The `<integer>` values inside the entry's `parameters` array, in order.
fn parameter_integers(entry: &str) -> Vec<i64> {
    let Some(p) = entry.find("<key>parameters</key>") else {
        return Vec::new();
    };
    let after = &entry[p..];
    let Some(arr_start) = after.find("<array>") else {
        return Vec::new();
    };
    let Some(arr_end) = after.find("</array>") else {
        return Vec::new();
    };
    if arr_end < arr_start {
        return Vec::new();
    }
    let arr = &after[arr_start..arr_end];
    let mut out = Vec::new();
    let mut rest = arr;
    while let Some(ip) = rest.find("<integer>") {
        let tail = &rest[ip + 9..];
        let Some(iend) = tail.find("</integer>") else {
            break;
        };
        if let Ok(v) = tail[..iend].trim().parse::<i64>() {
            out.push(v);
        }
        rest = &tail[iend..];
    }
    out
}

/// Read the live system table. macOS only; elsewhere returns empty (no
/// symbolichotkeys domain exists to conflict with).
pub fn read_system_hotkeys() -> Vec<SymbolicHotkey> {
    #[cfg(target_os = "macos")]
    {
        // `defaults export` rather than `defaults read`: export emits real
        // XML plist, read emits the old NeXT text format whose grammar is
        // even less pleasant to parse and changes with locale-ish quirks.
        let output = std::process::Command::new("defaults")
            .args(["export", "com.apple.symbolichotkeys", "-"])
            .output();
        match output {
            Ok(o) if o.status.success() => {
                parse_symbolichotkeys_xml(&String::from_utf8_lossy(&o.stdout))
            }
            // No domain / defaults failed: report nothing rather than
            // erroring; conflict detection is advisory.
            _ => Vec::new(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Convenience: live check of one chord against everything we can see.
pub fn check_chord(chord: &Chord) -> Vec<Conflict> {
    let mut out = find_conflicts(chord, &read_system_hotkeys());
    out.extend(platform_probe(chord));
    out
}

/// Platform-native conflict probes that are not table lookups.
///
/// Windows has the best conflict detection of the three platforms:
/// `RegisterHotKey` FAILS with `ERROR_HOTKEY_ALREADY_REGISTERED` when any
/// other process holds the chord, which is real knowledge rather than the
/// macOS situation (other apps' Carbon registrations are invisible). We
/// register, note the answer, and unregister immediately; the actual
/// binding is the low-level hook, so this leaves no state behind.
///
/// Bare modifiers are skipped because `RegisterHotKey` cannot express them,
/// and a probe that always fails would report a phantom conflict on the
/// product's default binding.
#[cfg(target_os = "windows")]
fn platform_probe(chord: &Chord) -> Vec<Conflict> {
    let Some(key) = chord.key else {
        return Vec::new();
    };
    if key.is_bare_modifier() {
        return Vec::new();
    }
    let Some(vk) = crate::winmatch::vk_for_key(key) else {
        return Vec::new();
    };
    if crate::backend::windows::chord_already_registered(vk, chord) {
        vec![Conflict {
            severity: Severity::Claimed,
            owner: "another running application (RegisterHotKey reports the chord is \
                    already held; both it and we will act on every press)"
                .to_string(),
        }]
    } else {
        Vec::new()
    }
}

#[cfg(not(target_os = "windows"))]
fn platform_probe(_chord: &Chord) -> Vec<Conflict> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed capture of real `defaults export com.apple.symbolichotkeys -`
    /// output: Spotlight (id 64, cmd+space, enabled), previous input source
    /// (id 60, ctrl+space, disabled), and a button-type entry with no
    /// parameters that must be skipped.
    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>AppleSymbolicHotKeys</key>
    <dict>
        <key>60</key>
        <dict>
            <key>enabled</key>
            <false/>
            <key>value</key>
            <dict>
                <key>parameters</key>
                <array>
                    <integer>32</integer>
                    <integer>49</integer>
                    <integer>262144</integer>
                </array>
                <key>type</key>
                <string>standard</string>
            </dict>
        </dict>
        <key>64</key>
        <dict>
            <key>enabled</key>
            <true/>
            <key>value</key>
            <dict>
                <key>parameters</key>
                <array>
                    <integer>32</integer>
                    <integer>49</integer>
                    <integer>1048576</integer>
                </array>
                <key>type</key>
                <string>standard</string>
            </dict>
        </dict>
        <key>10</key>
        <dict>
            <key>enabled</key>
            <true/>
            <key>value</key>
            <dict>
                <key>type</key>
                <string>button</string>
            </dict>
        </dict>
    </dict>
</dict>
</plist>
"#;

    #[test]
    fn parses_fixture() {
        let hks = parse_symbolichotkeys_xml(FIXTURE);
        assert_eq!(hks.len(), 2, "button entry skipped: {hks:?}");
        let spotlight = hks.iter().find(|h| h.id == 64).unwrap();
        assert!(spotlight.enabled);
        assert_eq!(spotlight.keycode, 49);
        assert_eq!(spotlight.modifiers, 1048576);
        let input_src = hks.iter().find(|h| h.id == 60).unwrap();
        assert!(!input_src.enabled);
    }

    #[test]
    fn cmd_space_collides_with_spotlight() {
        let hks = parse_symbolichotkeys_xml(FIXTURE);
        let chord: Chord = "cmd+space".parse().unwrap();
        let conflicts = find_conflicts(&chord, &hks);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].severity, Severity::Claimed);
        assert!(conflicts[0].owner.contains("Spotlight"), "{conflicts:?}");
    }

    #[test]
    fn ctrl_space_collides_but_disabled() {
        let hks = parse_symbolichotkeys_xml(FIXTURE);
        let chord: Chord = "ctrl+space".parse().unwrap();
        let conflicts = find_conflicts(&chord, &hks);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].severity, Severity::ClaimedButDisabled);
    }

    #[test]
    fn right_option_is_clean() {
        let hks = parse_symbolichotkeys_xml(FIXTURE);
        let chord = Chord::right_option();
        assert!(find_conflicts(&chord, &hks).is_empty());
    }

    #[test]
    fn bare_fn_warns_about_globe_action() {
        let conflicts = find_conflicts(&Chord::fn_key(), &[]);
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].owner.contains("Globe"), "{conflicts:?}");
    }

    #[test]
    fn cmd_tab_hits_static_table() {
        let chord: Chord = "cmd+tab".parse().unwrap();
        let conflicts = find_conflicts(&chord, &[]);
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].owner.contains("app switcher"));
    }

    #[test]
    fn unassigned_sentinel_never_matches() {
        // keycode 65535 means "no key"; it must not collide with anything,
        // including a chord whose key map might also fail.
        let hks = vec![SymbolicHotkey {
            id: 7,
            enabled: true,
            keycode: 65535,
            modifiers: 0,
        }];
        let chord: Chord = "cmd+space".parse().unwrap();
        assert!(find_conflicts(&chord, &hks).is_empty());
    }

    #[test]
    fn garbage_xml_yields_empty_not_panic() {
        assert!(parse_symbolichotkeys_xml("").is_empty());
        assert!(parse_symbolichotkeys_xml("<dict><key>64</key>").is_empty());
        assert!(parse_symbolichotkeys_xml("not xml at all").is_empty());
    }
}
