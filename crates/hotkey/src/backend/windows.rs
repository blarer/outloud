//! Windows backend: STUB. Documents the intended design so implementation
//! is a fill-in, not a redesign.
//!
//! Two viable mechanisms, and we will need BOTH:
//!
//! - **RegisterHotKey** is the polite API: the chord is registered with the
//!   OS, conflicts are detected for free (registration FAILS with
//!   ERROR_HOTKEY_ALREADY_REGISTERED if anything else holds the chord, which
//!   is better conflict detection than macOS offers). But it only delivers
//!   WM_HOTKEY on the DOWN edge; there is no release notification, so it
//!   cannot drive push-to-talk alone, and it cannot bind a bare modifier.
//! - **A low-level keyboard hook (SetWindowsHookExW, WH_KEYBOARD_LL)** sees
//!   every WM_KEYDOWN/WM_KEYUP system-wide, including bare modifiers with
//!   left/right VKs (VK_RMENU etc), which is what push-to-talk actually
//!   needs. It requires a message pump on the installing thread and has the
//!   same "don't be slow" trap as a CGEventTap: exceed the
//!   LowLevelHooksTimeout registry value and Windows silently unhooks you.
//!   No notification event exists for that, so the liveness watchdog must be
//!   a periodic self-check (inject a harmless event or track last-seen
//!   input timestamps via GetLastInputInfo).
//!
//! Plan: low-level hook for the PTT edges, plus a RegisterHotKey probe at
//! bind time purely as conflict detection (register, note success/failure,
//! immediately unregister).

use std::sync::mpsc::Sender;

use crate::matcher::Matcher;
use crate::taphold::TapHold;
use crate::{HotkeyError, HotkeyEvent};

pub fn spawn(
    _matcher: Matcher,
    _machine: TapHold,
    _sender: Sender<HotkeyEvent>,
) -> Result<(), HotkeyError> {
    Err(HotkeyError::Unsupported(
        "Windows backend not yet implemented (planned: WH_KEYBOARD_LL hook for edges, \
         RegisterHotKey probe for conflict detection)",
    ))
}
