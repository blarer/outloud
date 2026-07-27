//! Chord: a human-readable key binding ("cmd+shift+space", "fn",
//! "right-option") that parses from and displays back to the same string.
//!
//! Round-tripping matters because the chord string is what lives in the
//! user's config file and what error messages quote. If `parse(display(c))
//! != c` the settings UI and the running binding silently diverge, which is
//! exactly the "dead hotkey" failure class this crate exists to prevent.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

/// A modifier participating in a chord. Ordered so that `Display` output is
/// deterministic (BTreeSet iteration order == this enum's derive order),
/// matching the conventional macOS ordering ctrl-alt-shift-cmd, with fn
/// first because it is physically outermost on Apple keyboards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Modifier {
    /// The Fn/Globe key. See docs/hotkeys.md: it is not a normal key, it
    /// arrives as a flags change and may be remapped in System Settings.
    Fn,
    Control,
    Option,
    Shift,
    Command,
}

impl Modifier {
    fn token(self) -> &'static str {
        match self {
            Modifier::Fn => "fn",
            Modifier::Control => "ctrl",
            Modifier::Option => "alt",
            Modifier::Shift => "shift",
            Modifier::Command => "cmd",
        }
    }
}

/// The non-modifier part of a chord, or a *specific physical modifier key*
/// used as a bare binding (e.g. right Option for push-to-talk, the product
/// default per docs/ux/02-core-interaction.md). Bare modifiers get their own
/// variants rather than reusing [`Modifier`] because side matters for them:
/// binding "right-option" must not fire when the user holds left Option to
/// type an em dash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A printable character key: letters, digits, punctuation.
    Char(char),
    Space,
    Tab,
    Escape,
    Return,
    /// Function row / extended keys F1..=F19. F13-F19 exist because they are
    /// ideal PTT keys on full-size keyboards: nothing else claims them.
    F(u8),
    LeftCommand,
    RightCommand,
    LeftOption,
    RightOption,
    LeftControl,
    RightControl,
    LeftShift,
    RightShift,
}

impl Key {
    /// Whether this key is itself a modifier key (bound bare, side-specific).
    pub fn is_bare_modifier(self) -> bool {
        matches!(
            self,
            Key::LeftCommand
                | Key::RightCommand
                | Key::LeftOption
                | Key::RightOption
                | Key::LeftControl
                | Key::RightControl
                | Key::LeftShift
                | Key::RightShift
        )
    }

    fn token(self) -> String {
        match self {
            Key::Char(c) => c.to_string(),
            Key::Space => "space".into(),
            Key::Tab => "tab".into(),
            Key::Escape => "escape".into(),
            Key::Return => "return".into(),
            Key::F(n) => format!("f{n}"),
            Key::LeftCommand => "left-cmd".into(),
            Key::RightCommand => "right-cmd".into(),
            Key::LeftOption => "left-option".into(),
            Key::RightOption => "right-option".into(),
            Key::LeftControl => "left-ctrl".into(),
            Key::RightControl => "right-ctrl".into(),
            Key::LeftShift => "left-shift".into(),
            Key::RightShift => "right-shift".into(),
        }
    }
}

/// A parsed binding: a set of modifiers plus at most one key.
///
/// Three shapes are legal:
/// - modifiers + key ("cmd+shift+space"): classic chord
/// - bare `fn` (mods = {Fn}, key = None): the Aqua-style Globe binding
/// - bare side-specific modifier key ("right-option"): the product default
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub mods: BTreeSet<Modifier>,
    pub key: Option<Key>,
}

/// Why a chord string failed to parse. Carrying the offending token lets the
/// settings UI point at exactly what the user typed wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordParseError {
    Empty,
    UnknownToken(String),
    /// Two non-modifier keys, e.g. "a+b". One chord drives one binding.
    MultipleKeys(String, String),
    /// Modifiers with no key ("cmd+shift") is unbindable: there is no single
    /// key-up to anchor push-to-talk release on. `fn` alone is the exception,
    /// handled explicitly.
    ModifiersOnly,
    /// A bare-modifier key mixed with modifiers ("cmd+right-option") is
    /// rejected: the side-specific key IS the binding, combining it with
    /// flag-level modifiers creates chords no tap backend can report cleanly.
    BareModifierInChord,
    /// F-key out of the supported 1..=19 range.
    BadFunctionKey(String),
}

impl fmt::Display for ChordParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChordParseError::Empty => write!(f, "empty chord"),
            ChordParseError::UnknownToken(t) => write!(f, "unknown key token '{t}'"),
            ChordParseError::MultipleKeys(a, b) => {
                write!(f, "chord has two keys ('{a}' and '{b}'); bind one key")
            }
            ChordParseError::ModifiersOnly => write!(
                f,
                "modifier-only chords (other than 'fn' or a side-specific key like \
                 'right-option') cannot anchor a key-up; add a key"
            ),
            ChordParseError::BareModifierInChord => write!(
                f,
                "a side-specific modifier key ('right-option' etc.) must be bound alone"
            ),
            ChordParseError::BadFunctionKey(t) => {
                write!(f, "function key '{t}' out of range (f1..f19)")
            }
        }
    }
}

impl std::error::Error for ChordParseError {}

impl Chord {
    /// Bare Fn/Globe key, what Aqua Voice binds by default.
    pub fn fn_key() -> Chord {
        Chord {
            mods: [Modifier::Fn].into_iter().collect(),
            key: None,
        }
    }

    /// Right Option, this product's default (docs/ux/02-core-interaction.md).
    pub fn right_option() -> Chord {
        Chord {
            mods: BTreeSet::new(),
            key: Some(Key::RightOption),
        }
    }

    /// Whether the binding is a bare modifier (fn or a side-specific key),
    /// which the backends must observe via flags-changed events rather than
    /// key-down/key-up.
    pub fn is_bare_modifier(&self) -> bool {
        match self.key {
            Some(k) => k.is_bare_modifier(),
            None => self.mods.contains(&Modifier::Fn),
        }
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = self.mods.iter().map(|m| m.token().to_string()).collect();
        if let Some(k) = self.key {
            parts.push(k.token());
        }
        write!(f, "{}", parts.join("+"))
    }
}

impl FromStr for Chord {
    type Err = ChordParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut mods = BTreeSet::new();
        let mut key: Option<Key> = None;
        let tokens: Vec<&str> = s
            .split('+')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        if tokens.is_empty() {
            return Err(ChordParseError::Empty);
        }
        for tok in tokens {
            let lower = tok.to_ascii_lowercase();
            if let Some(m) = parse_modifier(&lower) {
                mods.insert(m);
                continue;
            }
            let k = parse_key(&lower)?;
            if let Some(prev) = key {
                return Err(ChordParseError::MultipleKeys(prev.token(), k.token()));
            }
            key = Some(k);
        }
        match key {
            Some(k) if k.is_bare_modifier() && !mods.is_empty() => {
                Err(ChordParseError::BareModifierInChord)
            }
            Some(k) => Ok(Chord { mods, key: Some(k) }),
            // "fn" alone is bindable (it is a flags change, both edges are
            // observable); "cmd+shift" alone is not.
            None if mods.len() == 1 && mods.contains(&Modifier::Fn) => {
                Ok(Chord { mods, key: None })
            }
            None => Err(ChordParseError::ModifiersOnly),
        }
    }
}

fn parse_modifier(tok: &str) -> Option<Modifier> {
    match tok {
        "cmd" | "command" | "super" | "meta" => Some(Modifier::Command),
        "shift" => Some(Modifier::Shift),
        "alt" | "opt" | "option" => Some(Modifier::Option),
        "ctrl" | "control" => Some(Modifier::Control),
        "fn" | "globe" => Some(Modifier::Fn),
        _ => None,
    }
}

fn parse_key(tok: &str) -> Result<Key, ChordParseError> {
    let key = match tok {
        "space" => Key::Space,
        "tab" => Key::Tab,
        "escape" | "esc" => Key::Escape,
        "return" | "enter" => Key::Return,
        "left-cmd" | "left-command" => Key::LeftCommand,
        "right-cmd" | "right-command" => Key::RightCommand,
        "left-option" | "left-alt" | "left-opt" => Key::LeftOption,
        "right-option" | "right-alt" | "right-opt" => Key::RightOption,
        "left-ctrl" | "left-control" => Key::LeftControl,
        "right-ctrl" | "right-control" => Key::RightControl,
        "left-shift" => Key::LeftShift,
        "right-shift" => Key::RightShift,
        t if t.len() == 1 => {
            let c = t.chars().next().expect("len checked");
            Key::Char(c)
        }
        t if t.starts_with('f') && t[1..].chars().all(|c| c.is_ascii_digit()) => {
            let n: u8 = t[1..]
                .parse()
                .map_err(|_| ChordParseError::BadFunctionKey(t.to_string()))?;
            if !(1..=19).contains(&n) {
                return Err(ChordParseError::BadFunctionKey(t.to_string()));
            }
            Key::F(n)
        }
        other => return Err(ChordParseError::UnknownToken(other.to_string())),
    };
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(s: &str) {
        let c: Chord = s.parse().expect(s);
        assert_eq!(c.to_string(), s, "display(parse({s:?}))");
        let c2: Chord = c.to_string().parse().expect("reparse");
        assert_eq!(c, c2);
    }

    #[test]
    fn roundtrips() {
        for s in [
            "fn",
            "shift+cmd+space",
            "ctrl+alt+shift+cmd+space",
            "right-option",
            "left-ctrl",
            "f13",
            "cmd+d",
            "fn+space",
        ] {
            roundtrip(s);
        }
    }

    #[test]
    fn parse_normalizes_aliases_and_order() {
        // Aliases and token order both normalize to one canonical display.
        let a: Chord = "shift+command+SPACE".parse().unwrap();
        assert_eq!(a.to_string(), "shift+cmd+space");
        let b: Chord = "globe".parse().unwrap();
        assert_eq!(b, Chord::fn_key());
        assert_eq!(b.to_string(), "fn");
    }

    #[test]
    fn bare_modifier_detection() {
        assert!(Chord::fn_key().is_bare_modifier());
        assert!(Chord::right_option().is_bare_modifier());
        let c: Chord = "cmd+shift+space".parse().unwrap();
        assert!(!c.is_bare_modifier());
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!("".parse::<Chord>(), Err(ChordParseError::Empty));
        assert_eq!(
            "cmd+shift".parse::<Chord>(),
            Err(ChordParseError::ModifiersOnly)
        );
        assert!(matches!(
            "a+b".parse::<Chord>(),
            Err(ChordParseError::MultipleKeys(_, _))
        ));
        assert_eq!(
            "cmd+right-option".parse::<Chord>(),
            Err(ChordParseError::BareModifierInChord)
        );
        assert!(matches!(
            "f0".parse::<Chord>(),
            Err(ChordParseError::BadFunctionKey(_))
        ));
        assert!(matches!(
            "f20".parse::<Chord>(),
            Err(ChordParseError::BadFunctionKey(_))
        ));
        assert!(matches!(
            "warp".parse::<Chord>(),
            Err(ChordParseError::UnknownToken(_))
        ));
    }
}
