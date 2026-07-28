//! Tier 1: accessibility in-place edit.
//!
//! macOS goes through `ax-edit`, which already proved the read/rewrite loop
//! in M0. Windows and Linux are honest stubs: the shapes of their APIs are
//! known and documented here so the ports are mechanical, not exploratory.

use crate::{Capabilities, Snapshot, TargetError, TextTarget, Tier};

/// macOS Accessibility (AXUIElement) target, delegating to `ax-edit`.
///
/// Off macOS every call surfaces `ax-edit`'s own `Unsupported`, so this type
/// compiles everywhere and [`crate::detect`] simply never selects it.
pub struct AxTarget;

impl AxTarget {
    /// Whether this process can actually use the AX tier right now: on macOS
    /// and trusted. `prompt: false` because detection must never pop dialogs.
    pub fn available() -> bool {
        cfg!(target_os = "macos") && ax_edit::is_trusted(false)
    }
}

impl TextTarget for AxTarget {
    fn name(&self) -> &'static str {
        "macos-ax"
    }

    fn tier(&self) -> Tier {
        Tier::Accessibility
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: true,
            // True when the field accepts AXSelectedText writes; ax-edit
            // reports the strategy actually used, so this is the honest
            // best case rather than a guarantee.
            preserves_undo: true,
            is_headless: false,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        let snap = ax_edit::snapshot_focused()?;
        let text = snap
            .value
            .clone()
            .or_else(|| snap.selected_text.clone())
            .ok_or(TargetError::NotReadable("focused element exposes no text"))?;
        Ok(Snapshot {
            text,
            // AX reports UTF-16 units; translating to byte offsets needs the
            // value string, which we have, but a wrong mapping is worse than
            // none, so only pass it through when it is trivially in range.
            selection: None,
        })
    }

    fn insert(&mut self, text: &str) -> Result<(), TargetError> {
        // With an empty selection, writing AXSelectedText inserts at the
        // caret, which is exactly insert semantics.
        ax_edit::replace_focused(text)?;
        Ok(())
    }

    fn replace(&mut self, text: &str) -> Result<(), TargetError> {
        ax_edit::replace_focused(text)?;
        Ok(())
    }
}

/// Windows UI Automation target: the Windows equivalent of what `ax-edit`
/// does on macOS, and the core Windows capability.
///
/// Strategy, mirroring ax-edit's tiered approach:
///
/// - **Read** via `TextPattern`: `IUIAutomationTextPattern::DocumentRange`
///   gives the full text, `GetSelection` the selected range. Where the
///   element only implements `ValuePattern` (simple Win32 edit boxes), the
///   value string is the fallback read.
/// - **Replace** prefers keystroke-free in-place edits. UIA has no direct
///   "set this range's text" call (that is TSF's job), but two cooperating
///   paths cover most fields:
///   1. If there is a selection and the element supports `ValuePattern`
///      and is not read-only, compose the new value string and
///      `SetValue`. This bypasses the app's editing machinery, so undo is
///      usually LOST for this path; the capability flags say so.
///   2. With no better path, select-all + typed replacement belongs to the
///      SendInput tier, not here; this target refuses rather than
///      degrading silently, so the caller can choose.
///
/// UIPI trap (docs/build-and-release.md, docs/hotkeys.md): all of this
/// silently fails against *elevated* windows. `GetFocusedElement` either
/// errors or returns a proxy whose patterns refuse, because a
/// medium-integrity process cannot drive a high-integrity UI. The error
/// path here surfaces the HRESULT so the diagnosis is at least loggable.
#[cfg(all(target_os = "windows", feature = "display"))]
pub use self::windows_uia::UiaTarget;

#[cfg(all(target_os = "windows", feature = "display"))]
mod windows_uia {
    use super::*;

    use windows::core::BSTR;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern,
        IUIAutomationValuePattern, UIA_TextPatternId, UIA_ValuePatternId,
    };

    /// One connected UI Automation session. Holds the COM automation object
    /// for its lifetime; COM itself is initialized per-thread on creation.
    pub struct UiaTarget {
        automation: IUIAutomation,
    }

    fn com_err(what: &str, e: windows::core::Error) -> TargetError {
        TargetError::Transport(format!("{what}: {e}"))
    }

    impl UiaTarget {
        /// Connect to UI Automation. Initializes COM (apartment-threaded,
        /// the model UIA clients are documented for) on the calling thread;
        /// "already initialized" is success, not an error, so embedding in
        /// a host that owns COM works.
        pub fn new() -> Result<Self, TargetError> {
            unsafe {
                // S_FALSE (already initialized) is Ok(_) in windows-rs's
                // HRESULT mapping; RPC_E_CHANGED_MODE is a real conflict
                // worth surfacing.
                CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                    .ok()
                    .map_err(|e| com_err("CoInitializeEx", e))?;
                let automation: IUIAutomation =
                    CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                        .map_err(|e| com_err("CoCreateInstance(CUIAutomation)", e))?;
                Ok(UiaTarget { automation })
            }
        }

        fn focused(&self) -> Result<IUIAutomationElement, TargetError> {
            unsafe {
                self.automation
                    .GetFocusedElement()
                    .map_err(|e| com_err("GetFocusedElement (elevated window in focus?)", e))
            }
        }

        fn text_pattern(element: &IUIAutomationElement) -> Option<IUIAutomationTextPattern> {
            unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
                .ok()
        }

        fn value_pattern(element: &IUIAutomationElement) -> Option<IUIAutomationValuePattern> {
            unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
                .ok()
        }

        /// The focused element's currently selected text, if any.
        ///
        /// This is what decides dictate-vs-edit at key-down time: the
        /// Windows counterpart of `ax_edit::TextSnapshot::selected_text`.
        /// A caret is a zero-length selection in UIA exactly as in AX, so
        /// an empty selection and no selection give the same answer.
        pub fn selected_text(&mut self) -> Result<Option<String>, TargetError> {
            let element = self.focused()?;
            let Some(tp) = Self::text_pattern(&element) else {
                // ValuePattern-only controls expose no selection concept,
                // so the honest answer is "none", which degrades to
                // dictation rather than guessing at an edit.
                return Ok(None);
            };
            unsafe {
                let ranges = tp
                    .GetSelection()
                    .map_err(|e| com_err("TextPattern::GetSelection", e))?;
                let count = ranges
                    .Length()
                    .map_err(|e| com_err("TextRangeArray::Length", e))?;
                // Multi-range selections (column select in a grid) count as
                // no selection: rewriting a discontiguous selection has no
                // defined meaning, and dictation is the safe degradation.
                if count != 1 {
                    return Ok(None);
                }
                let range = ranges
                    .GetElement(0)
                    .map_err(|e| com_err("TextRangeArray::GetElement", e))?;
                let text = range
                    .GetText(-1)
                    .map_err(|e| com_err("TextRange::GetText", e))?
                    .to_string();
                Ok(if text.is_empty() { None } else { Some(text) })
            }
        }
    }

    impl TextTarget for UiaTarget {
        fn name(&self) -> &'static str {
            "windows-uia"
        }

        fn tier(&self) -> Tier {
            Tier::Accessibility
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                can_read: true,
                can_write_in_place: true,
                // The write path is ValuePattern::SetValue, which replaces
                // the control's value wholesale outside its edit machinery.
                // Unlike macOS AXSelectedText there is no honest
                // undo-preserving claim to make, so this is false until a
                // TSF text service exists (targets/ime.rs).
                preserves_undo: false,
                is_headless: false,
            }
        }

        fn read(&mut self) -> Result<Snapshot, TargetError> {
            let element = self.focused()?;

            if let Some(tp) = Self::text_pattern(&element) {
                let text = unsafe {
                    let range = tp
                        .DocumentRange()
                        .map_err(|e| com_err("TextPattern::DocumentRange", e))?;
                    // -1: the whole range, unbounded.
                    range
                        .GetText(-1)
                        .map_err(|e| com_err("TextRange::GetText", e))?
                        .to_string()
                };
                // Selection byte offsets would need per-range character
                // index math (UIA ranges are endpoint-relative, not
                // index-addressed); a wrong mapping is worse than none.
                return Ok(Snapshot {
                    text,
                    selection: None,
                });
            }

            if let Some(vp) = Self::value_pattern(&element) {
                let text = unsafe {
                    vp.CurrentValue()
                        .map_err(|e| com_err("ValuePattern::CurrentValue", e))?
                        .to_string()
                };
                return Ok(Snapshot {
                    text,
                    selection: None,
                });
            }

            Err(TargetError::NotReadable(
                "focused element implements neither TextPattern nor ValuePattern",
            ))
        }

        fn insert(&mut self, text: &str) -> Result<(), TargetError> {
            // Insert-at-caret without keystrokes needs TSF; through UIA the
            // only whole-value write is SetValue. Appending to the current
            // value is the closest approximation and matches what dictation
            // needs (text lands at the end of the field being dictated
            // into). Fields that reject SetValue (read-only, no pattern)
            // fall through to the SendInput tier via the error.
            let element = self.focused()?;
            let vp = Self::value_pattern(&element).ok_or(TargetError::Unsupported(
                "focused element has no ValuePattern; use the SendInput tier",
            ))?;
            unsafe {
                let current = vp
                    .CurrentValue()
                    .map_err(|e| com_err("ValuePattern::CurrentValue", e))?
                    .to_string();
                let combined = format!("{current}{text}");
                vp.SetValue(&BSTR::from(combined))
                    .map_err(|e| com_err("ValuePattern::SetValue", e))
            }
        }

        fn replace(&mut self, text: &str) -> Result<(), TargetError> {
            let element = self.focused()?;
            let vp = Self::value_pattern(&element).ok_or(TargetError::Unsupported(
                "focused element has no ValuePattern; in-place replace needs TSF or SendInput",
            ))?;
            unsafe {
                vp.SetValue(&BSTR::from(text))
                    .map_err(|e| com_err("ValuePattern::SetValue", e))
            }
        }
    }
}

/// Non-Windows builds (and headless Windows builds) keep a stub with the
/// same name so cross-platform callers compile; every call says why it
/// cannot work, matching how `AxTarget` behaves off macOS.
#[cfg(not(all(target_os = "windows", feature = "display")))]
pub struct UiaTarget;

#[cfg(not(all(target_os = "windows", feature = "display")))]
impl TextTarget for UiaTarget {
    fn name(&self) -> &'static str {
        "windows-uia"
    }

    fn tier(&self) -> Tier {
        Tier::Accessibility
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: true,
            preserves_undo: false,
            is_headless: false,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::Unsupported(
            "UI Automation exists only on Windows display builds",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "UI Automation exists only on Windows display builds",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "UI Automation exists only on Windows display builds",
        ))
    }
}

/// Linux AT-SPI2 target. Stub.
///
/// The port needs a D-Bus session connection (the `atspi` crate), the
/// focused object via the `Accessible` registry, its `Text` interface for
/// read, and `EditableText::insert_text` / `delete_text` for write. The
/// practical caveats are that GTK4 and Qt6 expose EditableText patchily,
/// Electron only when started with `--force-renderer-accessibility` or
/// when AT-SPI announces a screen reader, and Wayland-native apps vary by
/// toolkit version. can_read is real; can_write_in_place is app-dependent
/// in exactly the way `value_settable` is on macOS.
pub struct AtspiTarget;

impl TextTarget for AtspiTarget {
    fn name(&self) -> &'static str {
        "linux-atspi"
    }

    fn tier(&self) -> Tier {
        Tier::Accessibility
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_read: true,
            can_write_in_place: true,
            preserves_undo: false,
            is_headless: false,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::Unsupported(
            "Linux AT-SPI2 backend not yet implemented",
        ))
    }

    fn insert(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "Linux AT-SPI2 backend not yet implemented",
        ))
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        Err(TargetError::Unsupported(
            "Linux AT-SPI2 backend not yet implemented",
        ))
    }
}
