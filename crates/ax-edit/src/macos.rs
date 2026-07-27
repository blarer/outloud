//! macOS implementation backed by `AXUIElement`.
//!
//! Everything here is `unsafe` FFI against the C accessibility API, so the
//! module keeps a hard rule: every Core Foundation object obtained from a
//! `Copy`/`Create` function is immediately wrapped in a type that releases it
//! on drop, and no raw pointer escapes this module.

use std::ffi::c_void;

use accessibility_sys::{
    kAXErrorSuccess, kAXFocusedApplicationAttribute, kAXFocusedUIElementAttribute,
    kAXNumberOfCharactersAttribute, kAXRoleAttribute, kAXSelectedTextAttribute,
    kAXSelectedTextRangeAttribute, kAXValueAttribute, kAXValueTypeAXError, kAXValueTypeCFRange,
    AXError, AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElementCopyAttributeValue,
    AXUIElementCopyMultipleAttributeValues, AXUIElementCreateApplication,
    AXUIElementCreateSystemWide, AXUIElementIsAttributeSettable, AXUIElementRef,
    AXUIElementSetAttributeValue, AXUIElementSetMessagingTimeout, AXValueGetType, AXValueGetValue,
    AXValueRef,
};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::array::CFArrayRef;
use core_foundation_sys::base::{CFRange, CFRelease, CFTypeRef};
use libc::pid_t;

use crate::{AxError as Error, FoundField, RewriteStrategy, ScanResult, TextSnapshot};

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

/// Read several attributes off an element in ONE synchronous IPC round trip.
///
/// Benchmarked motivation (M-series, macOS 26, TextEdit focused): each
/// `AXUIElementCopyAttributeValue` costs 22-30us of round trip regardless of
/// payload, so the five snapshot attributes cost ~135us read separately and
/// ~50us batched. On the cold path (first contact with the target process,
/// where each round trip is milliseconds) the saving multiplies.
///
/// Called with options=0, so an attribute the element does not support comes
/// back as an `AXValue` of type `kAXValueTypeAXError` in its slot rather than
/// failing the whole call. Those placeholders are mapped to `None`, preserving
/// the per-call behaviour where an absent attribute is a normal outcome.
fn copy_attributes_batched(
    element: AXUIElementRef,
    names: &[&str],
) -> Result<Vec<Option<Value>>, Error> {
    let cf_names: Vec<CFString> = names.iter().map(|n| CFString::new(n)).collect();
    let array = CFArray::from_CFTypes(&cf_names);
    let mut out: CFArrayRef = std::ptr::null();
    let code = unsafe {
        AXUIElementCopyMultipleAttributeValues(element, array.as_concrete_TypeRef(), 0, &mut out)
    };
    ax_result(code)?;
    if out.is_null() {
        return Err(Error::Api(code));
    }
    // The returned array is owned (+1) and its length always equals the number
    // of attributes requested; own it so every element is released with it.
    let values: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(out) };
    let mut result = Vec::with_capacity(names.len());
    for value in values.iter() {
        let raw = value.as_CFTypeRef();
        if raw.is_null() {
            result.push(None);
            continue;
        }
        // An unsupported attribute arrives as an AXValue wrapping an AXError.
        // AXValueGetType on a non-AXValue CFType would be undefined, so check
        // the type id first.
        let is_error_placeholder = unsafe {
            core_foundation_sys::base::CFGetTypeID(raw) == accessibility_sys::AXValueGetTypeID()
                && AXValueGetType(raw as AXValueRef) == kAXValueTypeAXError
        };
        if is_error_placeholder {
            result.push(None);
        } else {
            // Retain: the CFArray owns its elements and releases them when it
            // drops, so a Value that outlives this scope needs its own +1.
            unsafe { core_foundation_sys::base::CFRetain(raw) };
            result.push(Some(Value(raw)));
        }
    }
    Ok(result)
}

/// Interpret an owned attribute value as a string, if it is one.
fn value_as_string(value: &Value) -> Option<String> {
    let cf_type: CFType = unsafe { CFType::wrap_under_get_rule(value.0) };
    cf_type.downcast::<CFString>().map(|s| s.to_string())
}

/// Interpret an owned attribute value as a CFRange, if it is one.
fn value_as_range(value: &Value) -> Option<(usize, usize)> {
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
        return None;
    }
    Some((range.location as usize, range.length as usize))
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
/// The obvious approach, asking `AXUIElementCreateSystemWide()` directly for
/// `AXFocusedUIElement`, is what most examples show and it does not work: on
/// current macOS it returns `kAXErrorCannotComplete` even for a fully trusted
/// process, because that element is served by a separate, stricter path.
///
/// The route that does work is to resolve the focused *application* first and
/// ask it for its focused element.
fn focused_element() -> Result<Element, Error> {
    let app = frontmost_app_element().ok_or(Error::NoFocusedElement)?;
    focused_in(&app)
}

/// Ask a specific application for the element that holds keyboard focus.
fn focused_in(app: &Element) -> Result<Element, Error> {
    match copy_element_attribute(app.0, kAXFocusedUIElementAttribute) {
        Ok(Some(element)) => Ok(with_timeout(element)),
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

/// Configure an element so a hung target cannot block the caller, then return it.
fn with_timeout(element: Element) -> Element {
    unsafe { AXUIElementSetMessagingTimeout(element.0, MESSAGING_TIMEOUT_SECS) };
    element
}

/// Ask a Chromium- or Electron-based application to expose its accessibility
/// tree.
///
/// Chromium keeps its full accessibility tree switched off by default, because
/// building it is expensive and most processes never need it. It turns the tree
/// on when an assistive technology sets the private `AXManualAccessibility`
/// attribute on the application element.
///
/// Without this, every Chromium-derived application (Chrome, Edge, VS Code,
/// Slack, Discord, Notion, Spotify, and much of the desktop) reports no text
/// fields at all, which would wrongly appear to be a limitation of the
/// accessibility API rather than an opt-in that was never requested.
///
/// Failure is deliberately ignored: native applications have no such attribute
/// and will simply refuse, which is not an error.
fn enable_chromium_accessibility(app: AXUIElementRef) {
    let key = CFString::new("AXManualAccessibility");
    let value = CFBoolean::true_value();
    unsafe {
        AXUIElementSetAttributeValue(app, key.as_concrete_TypeRef(), value.as_CFTypeRef());
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
            let app = with_timeout(app);
            enable_chromium_accessibility(app.0);
            return Some(app);
        }
    }

    let pid = frontmost_pid()?;
    let app = Element(unsafe { AXUIElementCreateApplication(pid) });
    if app.0.is_null() {
        return None;
    }
    let app = with_timeout(app);
    enable_chromium_accessibility(app.0);
    Some(app)
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
    // Resolve the application once, then read both its name and its focused
    // field from that same element. Looking them up independently allows focus
    // to move in between, which yields a snapshot that describes two different
    // applications at once.
    let app_element = frontmost_app_element().ok_or(Error::NoFocusedElement)?;
    let app = app_title(&app_element);

    let focused = focused_in(&app_element)?;
    let el = focused.0;

    // One batched IPC round trip for all five attributes instead of five
    // separate ones. Measured on this machine: 22-30us per separate warm call
    // vs ~50us for the whole batch, and the gap widens dramatically on the
    // cold path where every round trip costs milliseconds. Some accessibility
    // servers may not implement the batch call, so a whole-call failure falls
    // back to per-attribute reads rather than reporting an error the old code
    // would not have produced.
    const ATTRS: [&str; 5] = [
        kAXRoleAttribute,
        kAXValueAttribute,
        kAXSelectedTextAttribute,
        kAXSelectedTextRangeAttribute,
        kAXNumberOfCharactersAttribute,
    ];
    let (role, value, selected_text, selection, has_char_count) =
        match copy_attributes_batched(el, &ATTRS) {
            Ok(values) => (
                values[0].as_ref().and_then(value_as_string),
                values[1].as_ref().and_then(value_as_string),
                values[2].as_ref().and_then(value_as_string),
                values[3].as_ref().and_then(value_as_range),
                values[4].is_some(),
            ),
            Err(_) => (
                copy_string_attribute(el, kAXRoleAttribute)?,
                copy_string_attribute(el, kAXValueAttribute)?,
                copy_string_attribute(el, kAXSelectedTextAttribute)?,
                copy_range_attribute(el, kAXSelectedTextRangeAttribute)?,
                copy_attribute(el, kAXNumberOfCharactersAttribute)?.is_some(),
            ),
        };
    let role = role.unwrap_or_else(|| "unknown".into());

    // A field with neither a value nor a selection nor a character count is not
    // a text field at all, so report that rather than a confusing empty edit.
    if value.is_none() && selected_text.is_none() && !has_char_count {
        return Err(Error::NoTextValue);
    }

    Ok(TextSnapshot {
        role,
        app,
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
    //
    // Prefer `TextSnapshot::app` where a snapshot is already in hand: it is
    // captured atomically with the field, whereas this call can race focus.
    let app = frontmost_app_element()?;
    app_title(&app)
}

/// Accessibility title of an application element, when it has a usable one.
fn app_title(app: &Element) -> Option<String> {
    copy_string_attribute(app.0, accessibility_sys::kAXTitleAttribute)
        .ok()
        .flatten()
        .filter(|title| !title.is_empty())
}

/// Walk a named application's accessibility tree and collect its text fields.
///
/// Used to verify coverage in applications that cannot easily be brought to the
/// front, and to answer the question "does this application expose anything we
/// can edit at all", which is otherwise indistinguishable from "the user had
/// nothing focused".
///
/// One limitation is worth knowing: `AXWindows` reports no windows for an
/// application whose windows all live on another Space. That is a property of
/// the window server, not of this code, and it does not affect the product,
/// which only ever acts on the focused application on the current Space. It
/// does mean an empty result here can mean either "exposes nothing" or "is
/// elsewhere", so the caller reports the window count alongside.
pub fn find_text_fields(app_name: &str, max_depth: usize) -> Result<ScanResult, Error> {
    let pid = pid_for_app_named(app_name).ok_or(Error::NoFocusedElement)?;
    let app = Element(unsafe { AXUIElementCreateApplication(pid) });
    if app.0.is_null() {
        return Err(Error::NoFocusedElement);
    }
    let app = with_timeout(app);

    // Chromium-derived applications expose nothing until asked, and the tree
    // takes a moment to be built once they are.
    enable_chromium_accessibility(app.0);
    std::thread::sleep(std::time::Duration::from_millis(400));

    // An application's windows are reached through `AXWindows`, not through
    // `AXChildren`: the child list of an application element holds its menu
    // bar. Walking children alone therefore finds nothing but menus, which is
    // both useless and slow.
    let mut fields = Vec::new();
    let mut window_count = 0;
    if let Some(windows) = copy_attribute(app.0, accessibility_sys::kAXWindowsAttribute)? {
        let windows: CFArray<CFType> =
            unsafe { CFArray::wrap_under_get_rule(windows.0 as CFArrayRef) };
        window_count = windows.len() as usize;
        for (index, window) in windows.iter().enumerate() {
            let mut path = vec![index];
            let window_ref = window.as_CFTypeRef() as AXUIElementRef;
            collect_text_fields(window_ref, &mut path, 0, max_depth, &mut fields)?;
        }
    }
    Ok(ScanResult {
        windows: window_count,
        fields,
    })
}

fn collect_text_fields(
    element: AXUIElementRef,
    path: &mut Vec<usize>,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<FoundField>,
) -> Result<(), Error> {
    if depth > max_depth {
        return Ok(());
    }

    let role = copy_string_attribute(element, kAXRoleAttribute)
        .ok()
        .flatten()
        .unwrap_or_default();

    if role == "AXTextArea" || role == "AXTextField" {
        out.push(FoundField {
            role: role.clone(),
            path: path.clone(),
            value: copy_string_attribute(element, kAXValueAttribute)
                .ok()
                .flatten(),
            settable: is_settable(element, kAXValueAttribute)
                || is_settable(element, kAXSelectedTextAttribute),
        });
        // A text field's descendants are its own internals, never further
        // fields, so there is nothing to gain by descending into it.
        return Ok(());
    }

    // Menus can hold thousands of elements and never contain an editable
    // field, so skipping them keeps a scan fast enough to be interactive.
    if role.starts_with("AXMenu") {
        return Ok(());
    }

    let Some(children) = copy_attribute(element, accessibility_sys::kAXChildrenAttribute)? else {
        return Ok(());
    };
    let children: CFArray<CFType> =
        unsafe { CFArray::wrap_under_get_rule(children.0 as CFArrayRef) };

    for (index, child) in children.iter().enumerate() {
        path.push(index);
        let child_ref = child.as_CFTypeRef() as AXUIElementRef;
        collect_text_fields(child_ref, path, depth + 1, max_depth, out)?;
        path.pop();
    }
    Ok(())
}

/// Process id of a running application, found by its accessibility title.
///
/// Walking the window list finds only applications with on-screen windows,
/// which is exactly the limitation this diagnostic exists to work around, so
/// the process list is consulted instead.
fn pid_for_app_named(name: &str) -> Option<pid_t> {
    let output = std::process::Command::new("pgrep")
        .arg("-f")
        .arg(format!("{name}.app/Contents/MacOS/"))
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<pid_t>().ok())
        .next()
}
