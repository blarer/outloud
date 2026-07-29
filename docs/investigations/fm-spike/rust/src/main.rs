//! Rust side of the Foundation Models feasibility spike.
//!
//! Links the Swift C-ABI shim and exercises both entry points, to confirm
//! the integration path recommended in docs/investigations/edit-intent.md is
//! real rather than assumed.
//!
//! The interesting assertion is the degradation one: on a machine where the
//! user has not enabled Apple Intelligence, `transform` must return null
//! promptly so the caller can fall back to llama.cpp. A hang or a trap here
//! would make Foundation Models unusable as a preferred backend.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::time::Instant;

extern "C" {
    fn outloud_fm_availability() -> i32;
    fn outloud_fm_transform(text: *const c_char, instruction: *const c_char) -> *mut c_char;
    fn outloud_fm_free(p: *mut c_char);
}

fn availability_label(code: i32) -> &'static str {
    match code {
        0 => "available",
        1 => "unavailable: Apple Intelligence not enabled by the user",
        2 => "unavailable: device not eligible",
        3 => "unavailable: model not ready (downloading)",
        _ => "unknown",
    }
}

fn main() {
    let t = Instant::now();
    let code = unsafe { outloud_fm_availability() };
    println!(
        "outloud_fm_availability() = {code} ({}) in {:?}",
        availability_label(code),
        t.elapsed()
    );

    let text = CString::new("we should probably ship the thing today i think").unwrap();
    let instruction = CString::new("tighten this up").unwrap();

    let t = Instant::now();
    let ptr = unsafe { outloud_fm_transform(text.as_ptr(), instruction.as_ptr()) };
    let elapsed = t.elapsed();

    if ptr.is_null() {
        println!("outloud_fm_transform() = NULL in {elapsed:?}");
        println!(
            "\nDegradation works: the call returned promptly instead of \
             hanging or trapping, so a Rust backend can fall through to \
             llama.cpp."
        );
    } else {
        let out = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        unsafe { outloud_fm_free(ptr) };
        println!("outloud_fm_transform() = {out:?} in {elapsed:?}");
    }
}
