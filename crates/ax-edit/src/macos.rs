//! macOS implementation backed by `AXUIElement`.
//!
//! Everything here is `unsafe` FFI against the C accessibility API, so the
//! module keeps a hard rule: every Core Foundation object obtained from a
//! `Copy`/`Create` function is immediately wrapped in a type that releases it
//! on drop, and no raw pointer escapes this module.

use std::ffi::c_void;

use accessibility_sys::{
    kAXErrorSuccess, kAXFocusedApplicationAttribute, kAXFocusedUIElementAttribute, kAXNumberOfCharactersAttribute,
    kAXRoleAttribute, kAXSelectedTextAttribute, kAXSelectedTextRangeAttribute, kAXValueAttribute,
    kAXValueTypeCFRange, AXError, AXIsProcessTrusted, AXIsProcessTrustedWithOptions,
    AXUIElementCopyAttributeValue, AXUIElementCreateApplication, AXUIElementCreateSystemWide,
    AXUIElementIsAttributeSettable,
    AXUIElementRef, AXUIElementSetAttributeValue, AXUIElementSetMessagingTimeout, AXValueGetValue,
    AXValueRef,
};
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::array::CFArrayRef;
use core_foundation_sys::base::{CFRange, CFRelease, CFTypeRef};
use libc::pid_t;

use crate::{AxError as Error, RewriteStrategy, TextSnapshot};

/// Owned `AXUIElementRef` that releases on drop.
///
/// The accessibility API hands back +1 references from every `Copy` call, and
/// leaking them leaks the target application's UI objects, so ownership is
/// modelled explicitly rather than by convention.
struct Element(AXUIElementRef);

impl Drop for Element {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0 as CFTypeRef) };
        }
    }
}

/// Owned `CFTypeRef` attribute value that releases on drop.
struct Value(CFTypeRef);

impl Drop for Value {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

fn ax_result(code: AXError) -> Result<(), Error> {
    if code == kAXErrorSuccess {
        Ok(())
    } else {
        Err(map_error(code))
    }
}

/// Translate a raw `AXError` into something the caller can act on.
///
/// `kAXErrorCannotComplete` deserves special handling: on the system-wide
/// element it almost always means the calling process is not actually trusted,
/// even when `AXIsProcessTrusted` has just returned true. That happens because
/// the trust check is cached per process while the real permission lives with
/// the *responsible* process, which for a helper binary launched by another
/// tool is the parent, not the binary itself. Reporting it as a generic API
/// failure sends people debugging the wrong thing.
fn map_error(code: AXError) -> Error {
    match code {
        accessibility_sys::kAXErrorCannotComplete => Error::NotTrusted,
        accessibility_sys::kAXErrorAPIDisabled => Error::NotTrusted,
        other => Error::Api(other),
    }
}

/// Cap how long a single accessibility call may block.
///
/// The API is synchronous IPC into another application. A busy or hung target
/// (a spinning Electron renderer is the usual culprit) would otherwise stall
/// the caller indefinitely, which in a dictation tool means the user's
/// keystroke appears to do nothing.
const MESSAGING_TIMEOUT_SECS: f32 = 0.5;

/// Copy one attribute off an element. `Ok(None)` means the attribute is simply
/// absent, which is normal and distinct from a hard API failure.
fn copy_attribute(element: AXUIElementRef, name: &str) -> Result<Option<Value>, Error> {
    let cf_name = CFString::new(name);
    let mut raw: CFTypeRef = std::ptr::null();
    let code = unsafe {
        AXUIElementCopyAttributeValue(
            element,
            cf_name.as_concrete_TypeRef(),
            &mut raw as *mut CFTypeRef,
        )
    };

    if code == kAXErrorSuccess {
        if raw.is_null() {
            return Ok(None);
        }
        return Ok(Some(Value(raw)));
    }

    // These codes all mean "this element does not offer that attribute", which
    // is an expected outcome for e.g. a button that has no AXValue.
    const NO_VALUE: [AXError; 3] = [
        accessibility_sys::kAXErrorNoValue,
        accessibility_sys::kAXErrorAttributeUnsupported,
        accessibility_sys::kAXErrorInvalidUIElement,
    ];
    if NO_VALUE.contains(&code) {
        Ok(None)
    } else {
        Err(map_error(code))
    }
}

fn copy_string_attribute(element: AXUIElementRef, name: &str) -> Result<Option<String>, Error> {
    let Some(value) = copy_attribute(element, name)? else {
        return Ok(None);
    };
    // Only interpret the value as a string when it really is one. Some apps
    // return an AXValue or a number for attributes we expect to be text.
    let cf_type: CFType = unsafe { CFType::wrap_under_get_rule(value.0) };
    Ok(cf_type
        .downcast::<CFString>()
        .map(|s: CFString| s.to_string()))
}

fn copy_element_attribute(element: AXUIElementRef, name: &str) -> Result<Option<Element>, Error> {
    let Some(value) = copy_attribute(element, name)? else {
        return Ok(None);
    };
    // Transfer ownership from `Value` to `Element` without an extra retain or
    // a double release.
    let raw = value.0 as AXUIElementRef;
    std::mem::forget(value);
    Ok(Some(Element(raw)))
}

/// Read `AXSelectedTextRange`, which arrives as an `AXValue` wrapping a
/// `CFRange` measured in UTF-16 code units.
fn copy_range_attribute(
    element: AXUIElementRef,
    name: &str,
) -> Result<Option<(usize, usize)>, Error> {
    let Some(value) = copy_attribute(element, name)? else {
        return Ok(None);
    };
    let mut range = CFRange {
        location: 0,
        length: 0,
    };
    let ok = unsafe {
        AXValueGetValue(
            value.0 as AXValueRef,
            kAXValueTypeCFRange,
            &mut range as *mut CFRange as *mut c_void,
        )
    };
    if !ok || range.location < 0 || range.length < 0 {
        return Ok(None);
    }
    Ok(Some((range.location as usize, range.length as usize)))
}

fn is_settable(element: AXUIElementRef, name: &str) -> bool {
    let cf_name = CFString::new(name);
    let mut settable = false;
    let code = unsafe {
        AXUIElementIsAttributeSettable(
            element,
            cf_name.as_concrete_TypeRef(),
            &mut settable as *mut bool,
        )
    };
    code == kAXErrorSuccess && settable
}

fn set_string_attribute(element: AXUIElementRef, name: &str, text: &str) -> Result<(), Error> {
    let cf_name = CFString::new(name);
    let cf_text = CFString::new(text);
    let code = unsafe {
        AXUIElementSetAttributeValue(
            element,
            cf_name.as_concrete_TypeRef(),
            cf_text.as_CFTypeRef(),
        )
    };
    ax_result(code)
}

pub fn is_trusted(prompt: bool) -> bool {
    if !prompt {
        return unsafe { AXIsProcessTrusted() };
    }
    // `kAXTrustedCheckOptionPrompt` is not re-exported by accessibility-sys, so
    // the key is constructed by name. It is stable public API.
    let key = CFString::new("AXTrustedCheckOptionPrompt");
    let value = CFNumber::from(1i32);
    let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
    unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) }
}

/// Resolve the element that currently owns keyboard focus.
///
/// There are two routes to the focused element and they are not equivalent.
///
/// The obvious one, `AXUIElementCreateSystemWide()`, is what most tutorials
/// show. On current macOS it frequently returns `kAXErrorCannotComplete` even
/// for a fully trusted process, because the system-wide element is served by a
/// separate path with its own stricter checks.
///
/// The reliable route is to identify the frontmost application, build an
/// element for *that process*, and ask it for its focused element. This is what
/// the shipping product must do. The system-wide element is still tried first,
/// since when it does work it saves a process lookup.
fn focused_element() -> Result<Element, Error> {
    if let Some(element) = focused_via_system_wide() {
        return Ok(element);
    }
    focused_via_frontmost_app()
}

/// Configure an element so a hung target cannot block the caller, then return it.
fn with_timeout(element: Element) -> Element {
    unsafe { AXUIElementSetMessagingTimeout(element.0, MESSAGING_TIMEOUT_SECS) };
    element
}

fn focused_via_system_wide() -> Option<Element> {
    let system = Element(unsafe { AXUIElementCreateSystemWide() });
    if system.0.is_null() {
        return None;
    }
    let system = with_timeout(system);
    copy_element_attribute(system.0, kAXFocusedUIElementAttribute)
        .ok()
        .flatten()
}

fn focused_via_frontmost_app() -> Result<Element, Error> {
    let app = frontmost_app_element().ok_or(Error::NoFocusedElement)?;

    match copy_element_attribute(app.0, kAXFocusedUIElementAttribute) {
        Ok(Some(element)) => Ok(element),
        // The application answered but reports nothing focused, which is a real
        // state (an app showing only a toolbar, for instance).
        Ok(None) => Err(Error::NoFocusedElement),
        // Only report a trust problem when the process genuinely is not
        // approved, so a permission message is never shown for an unrelated
        // failure.
        Err(Error::NotTrusted) if !unsafe { AXIsProcessTrusted() } => Err(Error::NotTrusted),
        Err(other) => Err(other),
    }
}

/// An `AXUIElement` for the application the user is currently working in.
///
/// Two routes are tried, in order of reliability.
///
/// The first asks the system-wide element for `AXFocusedApplication`. This is
/// authoritative, because it reflects genuine keyboard focus rather than window
/// stacking, and it is cheap.
///
/// The second falls back to the Core Graphics window list. That list is ordered
/// front to back by *window*, which is not the same as by application: a
/// floating panel or an overlay belonging to another process can sit in front
/// of the window the user is actually typing into. It is therefore only a
/// fallback, and it skips windows outside the normal application layer.
fn frontmost_app_element() -> Option<Element> {
    let system = Element(unsafe { AXUIElementCreateSystemWide() });
    if !system.0.is_null() {
        let system = with_timeout(system);
        if let Ok(Some(app)) = copy_element_attribute(system.0, kAXFocusedApplicationAttribute) {
            return Some(with_timeout(app));
        }
    }

    let pid = frontmost_pid()?;
    let app = Element(unsafe { AXUIElementCreateApplication(pid) });
    if app.0.is_null() {
        return None;
    }
    Some(with_timeout(app))
}

/// Process id of the frontmost application.
///
/// Resolved through the Core Graphics window list rather than AppKit, so the
/// crate stays free of an Objective-C runtime dependency and, more importantly,
/// free of a process spawn. This sits on the dictation hot path, where spawning
/// `osascript` would cost tens of milliseconds out of the latency budget.
///
/// The frontmost application is the owner of the window at the front of the
/// on-screen window list, which is what `CGWindowListCopyWindowInfo` returns
/// first when asked for on-screen windows in front-to-back order.
fn frontmost_pid() -> Option<pid_t> {
    use core_foundation::array::CFArray;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;

    const LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
    const LIST_OPTION_EXCLUDE_DESKTOP: u32 = 1 << 4;
    const NULL_WINDOW_ID: u32 = 0;

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
        // Layer 0 is the normal application layer. Menu bars, the Dock, and
        // overlay windows live above it and must not be mistaken for the
        // application the user is typing into.
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
            return Some(pid as pid_t);
        }
    }
    None
}

extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> CFArrayRef;
}

pub fn snapshot_focused() -> Result<TextSnapshot, Error> {
    let focused = focused_element()?;
    let el = focused.0;

    let role = copy_string_attribute(el, kAXRoleAttribute)?.unwrap_or_else(|| "unknown".into());
    let value = copy_string_attribute(el, kAXValueAttribute)?;
    let selected_text = copy_string_attribute(el, kAXSelectedTextAttribute)?;
    let selection = copy_range_attribute(el, kAXSelectedTextRangeAttribute)?;

    // A field with neither a value nor a selection nor a character count is not
    // a text field at all, so report that rather than a confusing empty edit.
    let has_char_count = copy_attribute(el, kAXNumberOfCharactersAttribute)?.is_some();
    if value.is_none() && selected_text.is_none() && !has_char_count {
        return Err(Error::NoTextValue);
    }

    Ok(TextSnapshot {
        role,
        value,
        selected_text,
        selection,
        value_settable: is_settable(el, kAXValueAttribute),
        selected_text_settable: is_settable(el, kAXSelectedTextAttribute),
    })
}

pub fn replace_focused(replacement: &str) -> Result<RewriteStrategy, Error> {
    let focused = focused_element()?;
    let el = focused.0;

    let has_selection = copy_string_attribute(el, kAXSelectedTextAttribute)?
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    // Writing AXSelectedText is preferred: it goes through the app's own text
    // system, so undo, autocomplete, and change notifications keep working.
    if has_selection && is_settable(el, kAXSelectedTextAttribute) {
        set_string_attribute(el, kAXSelectedTextAttribute, replacement)?;
        return Ok(RewriteStrategy::SetSelectedText);
    }

    // No selection: replacing the whole value is the documented way to rewrite
    // a simple field. It typically resets undo, which the caller compensates
    // for by keeping its own undo stack.
    if is_settable(el, kAXValueAttribute) {
        set_string_attribute(el, kAXValueAttribute, replacement)?;
        return Ok(RewriteStrategy::SetValue);
    }

    // Some apps (notably Electron and terminal emulators) expose text but
    // refuse writes. The caller must fall back to synthesizing a paste.
    Err(Error::NotSettable)
}

pub fn frontmost_app() -> Option<String> {
    // Identify the application by the accessibility title of its process, which
    // is available without spawning anything. A bundle identifier would be
    // preferable for keying formatting rules, but obtaining one requires
    // AppKit; the title is sufficient for the spike and the lookup can be
    // upgraded later without changing this signature.
    let app = frontmost_app_element()?;
    copy_string_attribute(app.0, accessibility_sys::kAXTitleAttribute)
        .ok()
        .flatten()
        .filter(|title| !title.is_empty())
}
