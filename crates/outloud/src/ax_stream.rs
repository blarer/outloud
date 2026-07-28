//! In-place streaming writes into the focused field's dictated region.
//!
//! `ax_edit::replace_focused` can only rewrite the selection or the whole
//! `AXValue`. Streaming needs a third primitive: *address a range and
//! replace it*, repeatedly, without rewriting the whole field (the UX doc
//! forbids whole-`AXValue` streaming: it destroys the caret and the host's
//! undo). That primitive is `AXSelectedTextRange` (settable on cooperative
//! fields) followed by `AXSelectedText`, which routes every write through
//! the target's own text system exactly like the preferred commit path.
//!
//! This module owns one dictated **region**: the byte span of text this
//! utterance has inserted at the caret. `stream::WriteCommand`s address the
//! region in region-local byte offsets; this module maps them to absolute
//! UTF-16 offsets (the unit AX speaks) and performs the two attribute
//! writes. The mapping is pure and unit-tested on every platform; only the
//! two `AXUIElementSetAttributeValue` calls are macOS.
//!
//! The focused element is resolved once, at session start, and held for the
//! whole utterance. Writes address that element even if focus moves, which
//! is the safe failure: text keeps landing where dictation started rather
//! than spraying into whatever window took focus mid-sentence.

#[cfg(not(target_os = "macos"))]
use ax_edit::TextSnapshot;

/// Why a field cannot take streamed writes. Not an error: the session
/// degrades to buffered commit-on-release, which is the product default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotStreamable {
    /// No caret (no focused field, a selection, or an unreadable field).
    NoCaret,
    /// The field refuses `AXSelectedTextRange` or `AXSelectedText` writes.
    NotSettable,
    /// Not macOS: no AX to stream through.
    Unsupported,
}

/// The pure offset math shared by the live writer and the tests: where in
/// the field (absolute UTF-16) does a region-local byte range land, given
/// what has been applied so far.
///
/// `applied` is the region text already in the field (excluding `lead`),
/// `lead` the one-time joining space inserted before the region.
fn abs_range(
    region_start_u16: usize,
    lead: &str,
    applied: &str,
    range: &std::ops::Range<usize>,
) -> (usize, usize) {
    let u16len = |s: &str| s.encode_utf16().count();
    let location = region_start_u16 + u16len(lead) + u16len(&applied[..range.start]);
    let length = u16len(&applied[range.start..range.end]);
    (location, length)
}

/// Decide the joining whitespace around the region, from the key-down
/// snapshot: a space before if the caret touches the end of a word, and a
/// trailing space at settle if it touches the start of one. Mirrors the
/// buffered path's `spliced_at_caret` so both modes join text identically.
fn joins(value: &str, caret_byte: usize) -> (&'static str, &'static str) {
    let lead = if value[..caret_byte]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace())
    {
        " "
    } else {
        ""
    };
    let trail = if value[caret_byte..]
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace())
    {
        " "
    } else {
        ""
    };
    (lead, trail)
}

#[cfg(target_os = "macos")]
pub use macos::AxRegion;

/// Probe-and-begin on non-macOS: nothing to stream through.
#[cfg(not(target_os = "macos"))]
pub struct AxRegion;

#[cfg(not(target_os = "macos"))]
impl AxRegion {
    pub fn begin(_snap: &TextSnapshot) -> Result<AxRegion, NotStreamable> {
        Err(NotStreamable::Unsupported)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{abs_range, joins, NotStreamable};
    use ax_edit::TextSnapshot;
    use std::ffi::c_void;

    use accessibility_sys::{
        kAXErrorSuccess, kAXFocusedApplicationAttribute, kAXFocusedUIElementAttribute,
        kAXSelectedTextAttribute, kAXSelectedTextRangeAttribute, kAXValueTypeCFRange,
        AXUIElementCopyAttributeValue, AXUIElementCreateSystemWide, AXUIElementIsAttributeSettable,
        AXUIElementRef, AXUIElementSetAttributeValue, AXValueCreate,
    };
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{CFRange, CFRelease, CFTypeRef};

    /// Owned element reference, released on drop (AX `Copy` calls are +1).
    struct Element(AXUIElementRef);
    impl Drop for Element {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0 as CFTypeRef) };
            }
        }
    }
    // AXUIElementRef is an immutable CFType handle; the writes go through
    // AX IPC which is thread-safe. Needed because the region lives on the
    // writer thread while being created on the event loop.
    unsafe impl Send for Element {}

    /// One utterance's dictated region in the field focused at key-down.
    pub struct AxRegion {
        element: Element,
        /// Absolute UTF-16 offset where the region begins (the caret at
        /// session start).
        start_u16: usize,
        /// Region text already written, excluding `lead`.
        applied: String,
        lead: &'static str,
        trail: &'static str,
    }

    impl AxRegion {
        /// Probe the focused field and open a region at its caret.
        ///
        /// `snap` is the key-down snapshot the pipeline already took; the
        /// probe adds two `IsAttributeSettable` calls (~50us warm), cheap
        /// against the utterance it enables.
        pub fn begin(snap: &TextSnapshot) -> Result<AxRegion, NotStreamable> {
            // A selection means edit mode; a missing value or caret means
            // the buffered path's clipboard fallback territory. Both are
            // handled by degrading, never by guessing at offsets.
            let value = snap.value.as_deref().ok_or(NotStreamable::NoCaret)?;
            let (loc, len) = snap.selection.ok_or(NotStreamable::NoCaret)?;
            if len != 0 {
                return Err(NotStreamable::NoCaret);
            }
            let caret_byte =
                crate::utf16_offset_to_byte(value, loc).ok_or(NotStreamable::NoCaret)?;
            let element = focused_element().ok_or(NotStreamable::NoCaret)?;
            // The write path needs BOTH attributes settable: the range to
            // address, the text to replace. `selected_text_settable` from
            // the snapshot covers the second; probe the first here.
            if !snap.selected_text_settable
                || !is_settable(element.0, kAXSelectedTextRangeAttribute)
            {
                return Err(NotStreamable::NotSettable);
            }
            let (lead, trail) = joins(value, caret_byte);
            Ok(AxRegion {
                element,
                start_u16: loc,
                applied: String::new(),
                lead,
                trail,
            })
        }

        /// Apply one region-local command: address the absolute range, then
        /// replace it through the app's own text system. ~0.5ms warm (two
        /// AX round trips), milliseconds in Chrome; always called from the
        /// writer thread, never the event loop.
        pub fn apply(&mut self, cmd: &stream::WriteCommand) -> Result<(), String> {
            let (range, insert) = match cmd {
                stream::WriteCommand::Append(s) => (self.applied.len()..self.applied.len(), s),
                stream::WriteCommand::Splice { range, insert } => (range.clone(), insert),
            };
            let (loc, len) = abs_range(self.start_u16, self.lead, &self.applied, &range);
            // The joining space rides with the first write so the region
            // never exists half-joined in the field.
            let text: String = if self.applied.is_empty() && range.start == 0 {
                format!("{}{}", self.lead, insert)
            } else {
                insert.clone()
            };
            set_range(self.element.0, loc, len)?;
            set_text(self.element.0, &text)?;
            self.applied.replace_range(range, insert);
            Ok(())
        }

        /// Whether anything has landed in the field yet. Decides whether a
        /// failed session may still fall back to the buffered write path
        /// (only when the field is untouched: a partial region plus a full
        /// buffered insert would duplicate text).
        pub fn wrote_any(&self) -> bool {
            !self.applied.is_empty()
        }

        /// Seal the region: append the trailing join space, if one is due
        /// and any text landed. Leaves the caret after the region, exactly
        /// where the buffered path leaves it.
        pub fn seal(&mut self) -> Result<(), String> {
            if self.applied.is_empty() || self.trail.is_empty() {
                return Ok(());
            }
            let end = self.applied.len()..self.applied.len();
            let (loc, len) = abs_range(self.start_u16, self.lead, &self.applied, &end);
            set_range(self.element.0, loc, len)?;
            set_text(self.element.0, self.trail)
        }
    }

    fn copy_element(parent: AXUIElementRef, name: &str) -> Option<Element> {
        let cf_name = CFString::new(name);
        let mut raw: CFTypeRef = std::ptr::null();
        let code = unsafe {
            AXUIElementCopyAttributeValue(parent, cf_name.as_concrete_TypeRef(), &mut raw)
        };
        (code == kAXErrorSuccess && !raw.is_null()).then(|| Element(raw as AXUIElementRef))
    }

    /// Focused element via the route that works: focused application ->
    /// its focused element. The system-wide `AXFocusedApplication` route is
    /// tried first; on machines where it fails environmentally with
    /// cannot-complete (a known trust-attribution failure, see
    /// docs/latency.md hypothesis 4) the frontmost application is resolved
    /// through the CG window list instead, mirroring ax-edit's fallback.
    fn focused_element() -> Option<Element> {
        let system = Element(unsafe { AXUIElementCreateSystemWide() });
        if let Some(app) = copy_element(system.0, kAXFocusedApplicationAttribute) {
            return copy_element(app.0, kAXFocusedUIElementAttribute);
        }
        let pid = frontmost_pid()?;
        let app = Element(unsafe { accessibility_sys::AXUIElementCreateApplication(pid) });
        if app.0.is_null() {
            return None;
        }
        copy_element(app.0, kAXFocusedUIElementAttribute)
    }

    /// PID of the frontmost normal-layer window's owner, from the CG window
    /// list (front-to-back). Same constants and layer filter as ax-edit's
    /// `frontmost_pid`; duplicated because ax-edit does not export raw
    /// element handles and this crate may not modify it.
    fn frontmost_pid() -> Option<libc::pid_t> {
        use core_foundation::array::CFArray;
        use core_foundation::base::CFType;
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::number::CFNumber;

        const LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
        const LIST_OPTION_EXCLUDE_DESKTOP: u32 = 1 << 4;
        const NULL_WINDOW_ID: u32 = 0;
        extern "C" {
            fn CGWindowListCopyWindowInfo(
                option: u32,
                relative_to_window: u32,
            ) -> core_foundation_sys::array::CFArrayRef;
        }

        let info = unsafe {
            CGWindowListCopyWindowInfo(
                LIST_OPTION_ON_SCREEN_ONLY | LIST_OPTION_EXCLUDE_DESKTOP,
                NULL_WINDOW_ID,
            )
        };
        if info.is_null() {
            return None;
        }
        let windows: CFArray<CFDictionary> = unsafe { CFArray::wrap_under_create_rule(info) };
        let layer_key = CFString::new("kCGWindowLayer");
        let pid_key = CFString::new("kCGWindowOwnerPID");
        for window in windows.iter() {
            let layer = window
                .find(layer_key.as_CFTypeRef() as *const _)
                .and_then(|v| unsafe { CFType::wrap_under_get_rule(*v) }.downcast::<CFNumber>())
                .and_then(|n| n.to_i64());
            if layer != Some(0) {
                continue;
            }
            if let Some(pid) = window
                .find(pid_key.as_CFTypeRef() as *const _)
                .and_then(|v| unsafe { CFType::wrap_under_get_rule(*v) }.downcast::<CFNumber>())
                .and_then(|n| n.to_i64())
            {
                return Some(pid as libc::pid_t);
            }
        }
        None
    }

    fn is_settable(element: AXUIElementRef, name: &str) -> bool {
        let cf_name = CFString::new(name);
        let mut settable = false;
        let code = unsafe {
            AXUIElementIsAttributeSettable(element, cf_name.as_concrete_TypeRef(), &mut settable)
        };
        code == kAXErrorSuccess && settable
    }

    fn set_range(element: AXUIElementRef, loc: usize, len: usize) -> Result<(), String> {
        let range = CFRange {
            location: loc as isize,
            length: len as isize,
        };
        let value =
            unsafe { AXValueCreate(kAXValueTypeCFRange, &range as *const _ as *const c_void) };
        if value.is_null() {
            return Err("could not create AXValue range".into());
        }
        let cf_name = CFString::new(kAXSelectedTextRangeAttribute);
        let code = unsafe {
            let code = AXUIElementSetAttributeValue(
                element,
                cf_name.as_concrete_TypeRef(),
                value as CFTypeRef,
            );
            CFRelease(value as CFTypeRef);
            code
        };
        (code == kAXErrorSuccess)
            .then_some(())
            .ok_or_else(|| format!("set AXSelectedTextRange failed (AXError {code})"))
    }

    fn set_text(element: AXUIElementRef, text: &str) -> Result<(), String> {
        let cf_name = CFString::new(kAXSelectedTextAttribute);
        let cf_text = CFString::new(text);
        let code = unsafe {
            AXUIElementSetAttributeValue(
                element,
                cf_name.as_concrete_TypeRef(),
                cf_text.as_CFTypeRef(),
            )
        };
        (code == kAXErrorSuccess)
            .then_some(())
            .ok_or_else(|| format!("set AXSelectedText failed (AXError {code})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abs_range_counts_utf16_not_bytes() {
        // Region after "héllo " (caret at u16 offset 6), lead space, with
        // "naïve" applied: splicing bytes 0..6 ("naïve" is 6 bytes) must
        // report 5 UTF-16 units at offset 7.
        let (loc, len) = abs_range(6, " ", "naïve", &(0..6));
        assert_eq!((loc, len), (7, 5));
    }

    #[test]
    fn abs_range_append_at_end() {
        let (loc, len) = abs_range(10, "", "hello", &(5..5));
        assert_eq!((loc, len), (15, 0));
    }

    #[test]
    fn joins_mirror_the_buffered_paths_spacing() {
        assert_eq!(joins("word", 4), (" ", ""));
        assert_eq!(joins("word ", 5), ("", ""));
        // Caret between "a" and " b": glue on the left only, the field
        // already provides the right-hand space.
        assert_eq!(joins("a b", 1), (" ", ""));
        assert_eq!(joins("", 0), ("", ""));
        assert_eq!(joins("ab", 1), (" ", " "));
    }

    #[test]
    fn surrogate_pairs_count_as_two_units() {
        // "𝄞" is 4 bytes, 2 UTF-16 units.
        let (loc, len) = abs_range(0, "", "𝄞x", &(0..4));
        assert_eq!((loc, len), (0, 2));
    }
}
