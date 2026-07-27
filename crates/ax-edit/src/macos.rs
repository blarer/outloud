//! macOS implementation backed by `AXUIElement`.
//!
//! Everything here is `unsafe` FFI against the C accessibility API, so the
//! module keeps a hard rule: every Core Foundation object obtained from a
//! `Copy`/`Create` function is immediately wrapped in a type that releases it
//! on drop, and no raw pointer escapes this module.

use std::ffi::c_void;

use accessibility_sys::{
    kAXErrorSuccess, kAXFocusedUIElementAttribute, kAXNumberOfCharactersAttribute,
    kAXRoleAttribute, kAXSelectedTextAttribute, kAXSelectedTextRangeAttribute, kAXValueAttribute,
    kAXValueTypeCFRange, AXError, AXIsProcessTrusted, AXIsProcessTrustedWithOptions,
    AXUIElementCopyAttributeValue, AXUIElementCreateSystemWide, AXUIElementIsAttributeSettable,
    AXUIElementRef, AXUIElementSetAttributeValue, AXUIElementSetMessagingTimeout, AXValueGetValue,
    AXValueRef,
};
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::{CFRange, CFRelease, CFTypeRef};

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

/// Resolve the element that currently owns keyboard focus, system-wide.
fn focused_element() -> Result<Element, Error> {
    if !unsafe { AXIsProcessTrusted() } {
        return Err(Error::NotTrusted);
    }
    let system = Element(unsafe { AXUIElementCreateSystemWide() });
    if system.0.is_null() {
        return Err(Error::NoFocusedElement);
    }
    // Bound every downstream call, so one unresponsive application cannot hang
    // the dictation hotkey. The timeout is inherited by elements obtained from
    // this one.
    unsafe { AXUIElementSetMessagingTimeout(system.0, MESSAGING_TIMEOUT_SECS) };
    copy_element_attribute(system.0, kAXFocusedUIElementAttribute)?.ok_or(Error::NoFocusedElement)
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
    // Read the frontmost bundle id without linking AppKit, by asking the
    // accessibility layer for the focused application's process and resolving
    // it through `NSRunningApplication` via a lightweight ObjC-free path.
    // `lsappinfo` is avoided here because it costs a process spawn per call.
    use std::process::Command;
    let out = Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to get bundle identifier of first application process whose frontmost is true")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}
