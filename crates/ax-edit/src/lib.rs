//! Read and rewrite the focused text field through the macOS Accessibility API.
//!
//! This is the capability that separates an "insert text at cursor" dictation
//! tool from a true edit-by-voice tool. To rewrite what the user already wrote,
//! we must be able to (a) read the current contents of the focused field,
//! (b) read the selection, and (c) write a replacement back in place.
//!
//! On platforms other than macOS every entry point returns
//! [`AxError::Unsupported`], so callers can compile and degrade gracefully.

use std::fmt;

/// What went wrong while talking to the accessibility layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AxError {
    /// The process has not been granted Accessibility permission.
    NotTrusted,
    /// No UI element currently has keyboard focus.
    NoFocusedElement,
    /// The focused element exists but exposes no readable text.
    NoTextValue,
    /// The focused element is read-only, so no rewrite is possible.
    NotSettable,
    /// The accessibility API returned a failure code.
    Api(i32),
    /// This platform has no accessibility backend implemented.
    Unsupported,
}

impl fmt::Display for AxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AxError::NotTrusted => write!(
                f,
                "accessibility permission not granted (System Settings > Privacy & Security > Accessibility)"
            ),
            AxError::NoFocusedElement => write!(f, "no focused UI element"),
            AxError::NoTextValue => write!(f, "focused element exposes no text value"),
            AxError::NotSettable => write!(f, "focused element is read-only"),
            AxError::Api(code) => write!(f, "accessibility API error {code}"),
            AxError::Unsupported => write!(f, "accessibility backend unsupported on this platform"),
        }
    }
}

impl std::error::Error for AxError {}

/// A snapshot of the focused text field at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSnapshot {
    /// Accessibility role, e.g. `AXTextArea` or `AXTextField`.
    pub role: String,
    /// Full text contents of the field, when the field exposes them.
    pub value: Option<String>,
    /// The currently selected substring, when there is a selection.
    pub selected_text: Option<String>,
    /// Selection as a `(location, length)` pair in UTF-16 code units, which is
    /// the unit the accessibility API itself uses.
    pub selection: Option<(usize, usize)>,
    /// Whether `AXValue` can be written, which decides if in-place rewrite is
    /// possible or whether we must fall back to clipboard paste.
    pub value_settable: bool,
    /// Whether `AXSelectedText` can be written. This is the preferred rewrite
    /// path because it preserves undo history in most applications.
    pub selected_text_settable: bool,
}

impl TextSnapshot {
    /// The text an edit command should operate on: the selection if there is
    /// one, otherwise the whole field.
    pub fn edit_target(&self) -> Option<&str> {
        match &self.selected_text {
            Some(sel) if !sel.is_empty() => Some(sel.as_str()),
            _ => self.value.as_deref(),
        }
    }

    /// Whether the edit applies to a selection rather than the entire field.
    pub fn is_selection_edit(&self) -> bool {
        matches!(&self.selected_text, Some(sel) if !sel.is_empty())
    }

    /// Which rewrite strategy this field supports, best first.
    pub fn strategy(&self) -> RewriteStrategy {
        if self.selected_text_settable {
            RewriteStrategy::SetSelectedText
        } else if self.value_settable {
            RewriteStrategy::SetValue
        } else {
            RewriteStrategy::ClipboardPaste
        }
    }
}

/// How a rewrite will be delivered to the focused field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteStrategy {
    /// Write `AXSelectedText`. Preserves undo in most apps. Preferred.
    SetSelectedText,
    /// Write the whole `AXValue`. Works in simple fields, usually clobbers undo.
    SetValue,
    /// Neither attribute is writable: synthesize a paste. Universal fallback.
    ClipboardPaste,
}

impl fmt::Display for RewriteStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RewriteStrategy::SetSelectedText => write!(f, "set-selected-text"),
            RewriteStrategy::SetValue => write!(f, "set-value"),
            RewriteStrategy::ClipboardPaste => write!(f, "clipboard-paste"),
        }
    }
}

#[cfg(target_os = "macos")]
mod macos;

/// Whether this process is trusted for accessibility.
///
/// When `prompt` is true and the process is untrusted, macOS shows the system
/// dialog that deep-links into System Settings.
pub fn is_trusted(prompt: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::is_trusted(prompt)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = prompt;
        false
    }
}

/// Read the currently focused text field.
pub fn snapshot_focused() -> Result<TextSnapshot, AxError> {
    #[cfg(target_os = "macos")]
    {
        macos::snapshot_focused()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(AxError::Unsupported)
    }
}

/// Replace the current selection, or the whole field when nothing is selected,
/// with `replacement`. Returns the strategy that was actually used.
pub fn replace_focused(replacement: &str) -> Result<RewriteStrategy, AxError> {
    #[cfg(target_os = "macos")]
    {
        macos::replace_focused(replacement)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = replacement;
        Err(AxError::Unsupported)
    }
}

/// Bundle identifier of the frontmost application, used to pick
/// destination-aware formatting rules (terminal vs. IDE vs. chat).
pub fn frontmost_app() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_app()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(value: Option<&str>, selected: Option<&str>) -> TextSnapshot {
        TextSnapshot {
            role: "AXTextArea".into(),
            value: value.map(str::to_string),
            selected_text: selected.map(str::to_string),
            selection: None,
            value_settable: true,
            selected_text_settable: true,
        }
    }

    #[test]
    fn selection_wins_over_full_value() {
        let s = snap(Some("hello world"), Some("world"));
        assert_eq!(s.edit_target(), Some("world"));
        assert!(s.is_selection_edit());
    }

    #[test]
    fn empty_selection_falls_back_to_value() {
        let s = snap(Some("hello world"), Some(""));
        assert_eq!(s.edit_target(), Some("hello world"));
        assert!(!s.is_selection_edit());
    }

    #[test]
    fn strategy_prefers_selected_text() {
        assert_eq!(
            snap(Some("x"), None).strategy(),
            RewriteStrategy::SetSelectedText
        );
    }

    #[test]
    fn strategy_falls_back_to_value_then_paste() {
        let mut s = snap(Some("x"), None);
        s.selected_text_settable = false;
        assert_eq!(s.strategy(), RewriteStrategy::SetValue);
        s.value_settable = false;
        assert_eq!(s.strategy(), RewriteStrategy::ClipboardPaste);
    }
}
