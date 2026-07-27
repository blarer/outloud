//! Read and rewrite text in whatever currently has the user's keyboard focus,
//! regardless of what kind of program that is.
//!
//! `ax-edit` proved that in-place rewrite works where an accessibility tree
//! exposes a writable text field. This crate generalizes that result: every
//! destination that accepts text input gets *some* path for delivering a
//! rewrite, and the ones that can also be read back get the full
//! edit-by-voice loop. The interesting destinations are the ones with no
//! accessibility text field at all, terminals above all, and environments
//! with no display server whatsoever.
//!
//! The tiers, best first:
//!
//! 1. **Accessibility** in-place edit (macOS AX today; Windows UIAutomation
//!    `TextPattern` and Linux AT-SPI2 are stubbed).
//! 2. **Input-method injection** (Wayland `zwp_input_method_v2`, Windows TSF,
//!    a macOS input method). Insert-only, but composes with any toolkit.
//! 3. **Synthetic keystrokes** (CGEvent, `SendInput`, uinput). Insert-only
//!    and lossy for non-keyboard characters, but nearly universal.
//! 4. **Clipboard paste with restore**. Universal, one write per edit.
//! 5. **Terminal-native** protocols: OSC 52, bracketed paste, tmux, screen,
//!    kitty remote control, iTerm2, WezTerm CLI, and shell line-editor state
//!    (`READLINE_LINE`, zsh `$BUFFER`, fish `commandline`). Several of these
//!    can *read the current line back*, which no higher tier can do for a
//!    terminal.
//! 6. **Headless**: no display server at all. A local daemon protocol any
//!    editor or terminal can speak, plus a stdin/stdout filter mode.
//!
//! [`detect`] probes the environment and returns the best available target.
//! Callers that want a specific transport construct it directly from
//! [`targets`].

use std::fmt;

pub mod detect;
pub mod escape;
pub mod targets;

pub use detect::{detect, detect_with_env, Env, SystemEnv};

/// Which delivery mechanism a target belongs to. Order is preference order:
/// a lower tier is chosen over a higher one when both are available, except
/// that terminal-native transports outrank everything else *inside a
/// terminal*, because the higher tiers cannot read a terminal at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Platform accessibility API, in-place read and write.
    Accessibility,
    /// Input-method / IME injection at the compositor or text-service layer.
    InputMethod,
    /// Synthesized keyboard events.
    SyntheticKeys,
    /// Clipboard write, paste keystroke, clipboard restore.
    Clipboard,
    /// Terminal control protocols and multiplexer IPC.
    TerminalNative,
    /// No display server: daemon socket or stdio filter.
    Headless,
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Tier::Accessibility => "accessibility",
            Tier::InputMethod => "input-method",
            Tier::SyntheticKeys => "synthetic-keys",
            Tier::Clipboard => "clipboard",
            Tier::TerminalNative => "terminal-native",
            Tier::Headless => "headless",
        };
        f.write_str(s)
    }
}

/// What a concrete target can actually do. Callers branch on this rather
/// than on the target's type, so a new transport never changes call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Can return the destination's current text. Without this only
    /// dictation (append) is possible, not edit-by-voice.
    pub can_read: bool,
    /// Can replace existing text rather than only inserting after it.
    pub can_write_in_place: bool,
    /// The rewrite lands through the destination's own editing machinery,
    /// so its undo history keeps working.
    pub preserves_undo: bool,
    /// Works with no display server attached.
    pub is_headless: bool,
}

impl Capabilities {
    /// Insert-only transport: text goes in, nothing comes back.
    pub const fn insert_only(is_headless: bool) -> Self {
        Capabilities {
            can_read: false,
            can_write_in_place: false,
            preserves_undo: false,
            is_headless,
        }
    }
}

/// The text a target could see at one instant.
///
/// Deliberately smaller than `ax_edit::TextSnapshot`: most transports have no
/// concept of a role or a settable attribute, and forcing them to fake one
/// would push accessibility details into code that has none.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// Current contents of the edit region (a field, a command line, a pane).
    pub text: String,
    /// Cursor or selection as byte offsets into `text`, when known.
    pub selection: Option<(usize, usize)>,
}

/// Why a target operation failed.
#[derive(Debug)]
pub enum TargetError {
    /// The operation is not implemented for this transport, with the reason
    /// a caller can show a user ("kitty needs allow_remote_control").
    Unsupported(&'static str),
    /// The transport exists here but reading is not part of its protocol.
    NotReadable(&'static str),
    /// An external command or connection failed.
    Transport(String),
    /// Underlying accessibility error, passed through unmodified so callers
    /// keep the precise diagnosis (`NotTrusted` vs `NoFocusedElement`).
    Ax(ax_edit::AxError),
    Io(std::io::Error),
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetError::Unsupported(why) => write!(f, "unsupported: {why}"),
            TargetError::NotReadable(why) => write!(f, "not readable: {why}"),
            TargetError::Transport(why) => write!(f, "transport failed: {why}"),
            TargetError::Ax(e) => write!(f, "accessibility: {e}"),
            TargetError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for TargetError {}

impl From<std::io::Error> for TargetError {
    fn from(e: std::io::Error) -> Self {
        TargetError::Io(e)
    }
}

impl From<ax_edit::AxError> for TargetError {
    fn from(e: ax_edit::AxError) -> Self {
        TargetError::Ax(e)
    }
}

/// One way of getting text into (and ideally out of) the focused destination.
///
/// `&mut self` throughout: several transports hold state across the two
/// halves of an edit (a saved clipboard, an open socket), and a shared
/// reference would make that state a lie.
pub trait TextTarget {
    /// Short stable identifier, e.g. `"tmux"`, for logs and the compat matrix.
    fn name(&self) -> &'static str;

    fn tier(&self) -> Tier;

    fn capabilities(&self) -> Capabilities;

    /// Read the destination's current text. Targets whose capabilities say
    /// `can_read: false` return [`TargetError::NotReadable`].
    fn read(&mut self) -> Result<Snapshot, TargetError>;

    /// Insert `text` at the destination's cursor.
    fn insert(&mut self, text: &str) -> Result<(), TargetError>;

    /// Replace the destination's current edit region with `text`. Targets
    /// that cannot address existing text return
    /// [`TargetError::Unsupported`]; callers then decide whether an insert
    /// is an acceptable degradation, because for edit-by-voice it usually
    /// is not.
    fn replace(&mut self, text: &str) -> Result<(), TargetError>;
}
