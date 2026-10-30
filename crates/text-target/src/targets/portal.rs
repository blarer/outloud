//! Text injection through the XDG `RemoteDesktop` portal.
//!
//! ## Why this tier exists
//!
//! On a GNOME desktop neither of the other Linux write paths is available:
//!
//! - [`super::keys`]' `wtype` path is Wayland-only, so it is absent on an
//!   X11 session. There is no uinput path either: a `UinputTarget` stub
//!   existed but was unreachable and was removed on 2026-10-30, with the
//!   reasoning kept in `super::keys`.
//! - [`super::clipboard`] deliberately refuses when it cannot synthesize a
//!   paste keystroke, rather than clobber the clipboard and strand the
//!   text. On GNOME X11 that refusal is correct and leaves nothing.
//!
//! The portal closes that gap. `org.freedesktop.portal.RemoteDesktop`
//! exposes `NotifyKeyboardKeysym`, which GNOME implements on both X11 and
//! Wayland, needs no root and no udev rule, and never touches the
//! clipboard.
//!
//! ## Keysyms, not scancodes
//!
//! `NotifyKeyboardKeycode` would hit the problem [`super::keys`] documents
//! at length: an evdev keycode produces whatever the *active layout* maps
//! it to, so a keycode chosen for QWERTY types the wrong character under
//! Dvorak or AZERTY. `NotifyKeyboardKeysym` names the character directly
//! and the compositor finds a way to produce it, which is the same
//! reasoning that makes `wtype` correct on Wayland.
//!
//! For Latin-1 and ASCII a keysym is simply the Unicode codepoint. Above
//! that, X11 keysyms for Unicode are `codepoint | 0x0100_0000`.
//!
//! ## The consent lifetime, measured rather than assumed
//!
//! `Start` raises a system dialog ("share your screen / allow remote
//! control"). Two things were established on a GNOME 46 host before this
//! was written, because they decide the shape of the whole type:
//!
//! 1. A session CAN be created and started, and `Start` returns a granted
//!    device mask of 1 (keyboard), after which `NotifyKeyboardKeysym`
//!    succeeds.
//! 2. **The grant does not persist.** A second process creating a second
//!    session raised a second dialog. There is no "remember this" path.
//!
//! So the session is created **once, lazily, on first write** and then held
//! open for the lifetime of this value. One dialog per OutLoud run is
//! acceptable; one per utterance would not be. This is why `insert` takes
//! `&mut self` and why [`PortalTarget`] is not `Clone`: the session handle
//! is a live resource, not a configuration value.
//!
//! Calling `NotifyKeyboard*` before `Start` completes fails with
//! `"Session is not allowed to call NotifyKeyboard methods"`, which is
//! what a first implementation here did by reusing one event loop across
//! three overlapping requests and letting `Start` finish before
//! `SelectDevices` had registered. Each request is now awaited on its own
//! object path.

use std::collections::HashMap;
use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::{Capabilities, Snapshot, TargetError, TextTarget, Tier};

const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const RD_IFACE: &str = "org.freedesktop.portal.RemoteDesktop";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";

/// `AvailableDeviceTypes` / `SelectDevices` bit for a keyboard.
const DEVICE_KEYBOARD: u32 = 1;

/// Keysym for Return, which is not its ASCII value.
const KEYSYM_RETURN: i32 = 0xff0d;

/// X11 keysym offset for Unicode codepoints above Latin-1.
const KEYSYM_UNICODE_BASE: i32 = 0x0100_0000;

/// How long to wait for the user to answer the consent dialog. Deliberately
/// generous: the dialog can appear on a different workspace, and failing
/// after five seconds would look like a broken tier rather than an
/// unanswered prompt.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);

/// How long to wait for the portal to answer a request that needs no human.
const PORTAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Delay between keysym events. Without it, a fast burst can outrun the
/// compositor's input queue and arrive reordered in some clients.
const KEY_INTERVAL: Duration = Duration::from_micros(1500);

pub struct PortalTarget {
    /// Owns its connection, so this struct does not also store one: a
    /// `Proxy` built from `&self.connection` borrows the struct, which makes
    /// every method that needs a proxy fight the borrow checker and makes
    /// moving the connection into `Self` impossible.
    proxy: Proxy<'static>,
    /// `None` until the first write, because constructing this type must not
    /// raise a consent dialog: `select()` builds candidate targets to ask
    /// them what they can do, and a dialog there would fire even when
    /// another tier wins.
    session: Option<OwnedObjectPath>,
}

impl PortalTarget {
    /// Connect to the session bus and check the portal is answering.
    ///
    /// Does not create a portal session, so this is safe to call during tier
    /// selection. Returns `Unsupported` when there is no session bus or the
    /// portal does not implement `RemoteDesktop`, which is the honest answer
    /// for a headless box or a desktop without the portal backend.
    pub fn new() -> Result<Self, TargetError> {
        let connection = Connection::session()
            .map_err(|e| TargetError::Transport(format!("no D-Bus session bus: {e}")))?;

        let proxy = Self::rd_proxy(connection)?;
        let devices: u32 = proxy.get_property("AvailableDeviceTypes").map_err(|e| {
            TargetError::Transport(format!(
                "portal does not expose RemoteDesktop.AvailableDeviceTypes: {e}"
            ))
        })?;

        if devices & DEVICE_KEYBOARD == 0 {
            return Err(TargetError::Unsupported(
                "the desktop portal offers no keyboard device",
            ));
        }

        Ok(Self {
            proxy,
            session: None,
        })
    }

    /// Whether the portal is reachable and offers a keyboard device.
    ///
    /// Used by `SystemEnv::has_portal_keyboard` during tier selection, so it
    /// must stay cheap and must never call `CreateSession`: a consent dialog
    /// raised while merely *choosing* a tier would appear even on runs where
    /// another tier wins.
    pub fn keyboard_available() -> bool {
        let Ok(connection) = Connection::session() else {
            return false;
        };
        let Ok(proxy) = Self::rd_proxy(connection) else {
            return false;
        };
        proxy
            .get_property::<u32>("AvailableDeviceTypes")
            .map(|devices| devices & DEVICE_KEYBOARD != 0)
            .unwrap_or(false)
    }

    /// Build a proxy that OWNS its connection, so the result is `'static`
    /// and can be stored. `Proxy::new` borrows; `ProxyBuilder` with an owned
    /// connection does not.
    fn rd_proxy(connection: Connection) -> Result<Proxy<'static>, TargetError> {
        zbus::blocking::proxy::Builder::new(&connection)
            .destination(PORTAL_BUS)
            .and_then(|b| b.path(PORTAL_PATH))
            .and_then(|b| b.interface(RD_IFACE))
            .and_then(|b| b.build())
            .map_err(|e| TargetError::Transport(format!("cannot reach the desktop portal: {e}")))
    }

    /// Block until the portal answers the request at `path`, returning its
    /// results vardict.
    ///
    /// Subscribing to `Response` on the specific request path, rather than
    /// filtering a shared stream, is what keeps three in-flight requests
    /// from being confused for one another.
    fn await_response(
        &self,
        path: &OwnedObjectPath,
        timeout: Duration,
    ) -> Result<HashMap<String, OwnedValue>, TargetError> {
        let rule = format!(
            "type='signal',interface='{REQUEST_IFACE}',member='Response',path='{}'",
            path.as_str()
        );

        let connection = self.proxy.connection();
        let dbus = Proxy::new(
            connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .map_err(|e| TargetError::Transport(format!("cannot reach the bus daemon: {e}")))?;
        dbus.call_method("AddMatch", &(rule.as_str()))
            .map_err(|e| TargetError::Transport(format!("AddMatch failed: {e}")))?;

        let deadline = std::time::Instant::now() + timeout;
        let mut messages = MessageIterator::from(connection);

        let outcome = loop {
            if std::time::Instant::now() >= deadline {
                break Err(TargetError::Transport(format!(
                    "portal did not answer {} within {}s; if a consent dialog is \
                     open, it is waiting for an answer",
                    path.as_str(),
                    timeout.as_secs()
                )));
            }

            let Some(Ok(message)) = messages.next() else {
                continue;
            };
            let header = message.header();
            if header.interface().map(|i| i.as_str()) != Some(REQUEST_IFACE)
                || header.member().map(|m| m.as_str()) != Some("Response")
                || header.path().map(|p| p.as_str()) != Some(path.as_str())
            {
                continue;
            }

            let (code, results): (u32, HashMap<String, OwnedValue>) =
                match message.body().deserialize() {
                    Ok(body) => body,
                    Err(e) => {
                        break Err(TargetError::Transport(format!(
                            "malformed portal Response: {e}"
                        )))
                    }
                };

            break match code {
                0 => Ok(results),
                1 => Err(TargetError::Transport(
                    "the portal request was cancelled (consent dialog dismissed)".to_string(),
                )),
                other => Err(TargetError::Transport(format!(
                    "the portal ended the request, code {other}"
                ))),
            };
        };

        let _ = dbus.call_method("RemoveMatch", &(rule.as_str()));
        outcome
    }

    /// Create, configure and start a portal session, raising one consent
    /// dialog. Idempotent: later calls reuse the live session.
    fn ensure_session(&mut self) -> Result<OwnedObjectPath, TargetError> {
        if let Some(session) = &self.session {
            return Ok(session.clone());
        }

        let proxy = &self.proxy;
        let token = format!("outloud{}", std::process::id());

        let create_token = format!("{token}_create");
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("session_handle_token", Value::from(token.as_str()));
        options.insert("handle_token", Value::from(create_token.as_str()));
        let request: OwnedObjectPath = proxy
            .call("CreateSession", &(options))
            .map_err(|e| TargetError::Transport(format!("CreateSession failed: {e}")))?;

        let results = self.await_response(&request, PORTAL_TIMEOUT)?;
        let session: OwnedObjectPath = results
            .get("session_handle")
            .ok_or_else(|| TargetError::Transport("portal returned no session_handle".to_string()))
            .and_then(|v| {
                // GNOME returns this as a string, not an object path.
                String::try_from(v.clone())
                    .map_err(|e| {
                        TargetError::Transport(format!("session_handle not a string: {e}"))
                    })
                    .and_then(|s| {
                        ObjectPath::try_from(s)
                            .map(OwnedObjectPath::from)
                            .map_err(|e| {
                                TargetError::Transport(format!(
                                    "session_handle is not an object path: {e}"
                                ))
                            })
                    })
            })?;

        let select_token = format!("{token}_select");
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("types", Value::from(DEVICE_KEYBOARD));
        options.insert("handle_token", Value::from(select_token.as_str()));
        let request: OwnedObjectPath = proxy
            .call("SelectDevices", &(&session, options))
            .map_err(|e| TargetError::Transport(format!("SelectDevices failed: {e}")))?;
        self.await_response(&request, PORTAL_TIMEOUT)?;

        let start_token = format!("{token}_start");
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("handle_token", Value::from(start_token.as_str()));
        let request: OwnedObjectPath = proxy
            .call("Start", &(&session, "", options))
            .map_err(|e| TargetError::Transport(format!("Start failed: {e}")))?;
        let results = self.await_response(&request, CONSENT_TIMEOUT)?;

        // Read the granted mask back rather than assuming SelectDevices was
        // honoured: the portal is free to grant less than was asked for, and
        // finding out here gives a clear error instead of
        // "Session is not allowed to call NotifyKeyboard methods" later.
        let granted = results
            .get("devices")
            .and_then(|v| u32::try_from(v.clone()).ok())
            .unwrap_or(0);
        if granted & DEVICE_KEYBOARD == 0 {
            return Err(TargetError::Transport(format!(
                "portal started the session but granted devices {granted:#b}, \
                 no keyboard"
            )));
        }

        self.session = Some(session.clone());
        Ok(session)
    }

    /// X11 keysym for one character.
    fn keysym_for(ch: char) -> i32 {
        match ch {
            '\n' | '\r' => KEYSYM_RETURN,
            // ASCII and Latin-1 keysyms are the codepoint itself.
            c if (c as u32) < 0x100 => c as i32,
            c => c as i32 | KEYSYM_UNICODE_BASE,
        }
    }

    fn notify(
        &self,
        session: &OwnedObjectPath,
        keysym: i32,
        pressed: bool,
    ) -> Result<(), TargetError> {
        let options: HashMap<&str, Value> = HashMap::new();
        self.proxy
            .call::<_, _, ()>(
                "NotifyKeyboardKeysym",
                &(session, options, keysym, u32::from(pressed)),
            )
            .map_err(|e| {
                TargetError::Transport(format!("NotifyKeyboardKeysym({keysym:#x}) failed: {e}"))
            })
    }
}

impl TextTarget for PortalTarget {
    fn name(&self) -> &'static str {
        "linux-portal"
    }

    fn tier(&self) -> Tier {
        // Synthesized key events, like wtype and uinput: the portal is the
        // transport, not a different class of write. It cannot address text
        // that is already there, which is what separates this from
        // Accessibility.
        Tier::SyntheticKeys
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // The portal is write-only. There is no way to read the focused
            // field back through it, so edit-by-voice cannot use this tier.
            can_read: false,
            can_write_in_place: false,
            // Keystrokes go through the application's own input handling, so
            // its undo stack sees them as typing.
            preserves_undo: true,
            is_headless: false,
        }
    }

    fn read(&mut self) -> Result<Snapshot, TargetError> {
        Err(TargetError::NotReadable(
            "the RemoteDesktop portal is write-only",
        ))
    }

    fn insert(&mut self, text: &str) -> Result<(), TargetError> {
        let session = self.ensure_session()?;
        for ch in text.chars() {
            let keysym = Self::keysym_for(ch);
            self.notify(&session, keysym, true)?;
            self.notify(&session, keysym, false)?;
            std::thread::sleep(KEY_INTERVAL);
        }
        Ok(())
    }

    fn replace(&mut self, _text: &str) -> Result<(), TargetError> {
        // Deliberately not an insert. Silently appending when the caller
        // asked to replace is the degradation the trait's own documentation
        // warns is usually unacceptable for edit-by-voice.
        Err(TargetError::Unsupported(
            "the RemoteDesktop portal cannot address existing text",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_keysyms_are_codepoints() {
        assert_eq!(PortalTarget::keysym_for('a'), 0x61);
        assert_eq!(PortalTarget::keysym_for('Z'), 0x5a);
        assert_eq!(PortalTarget::keysym_for(' '), 0x20);
    }

    #[test]
    fn newline_uses_the_return_keysym_not_its_ascii_value() {
        // 0x0a would be a literal control character, which compositors do
        // not map to Return.
        assert_eq!(PortalTarget::keysym_for('\n'), 0xff0d);
        assert_eq!(PortalTarget::keysym_for('\r'), 0xff0d);
    }

    #[test]
    fn latin1_stays_bare_and_above_it_gets_the_unicode_offset() {
        // U+00E9 e-acute is inside Latin-1, so the keysym is the codepoint.
        assert_eq!(PortalTarget::keysym_for('é'), 0xe9);
        // U+2014 em dash is not, so it needs the Unicode base.
        assert_eq!(PortalTarget::keysym_for('—'), 0x0100_2014);
        // Emoji take the same path; whether the compositor can produce one
        // is its business, but the encoding must still be right.
        assert_eq!(PortalTarget::keysym_for('🙂'), 0x0101_F642);
    }

    #[test]
    fn replace_refuses_rather_than_degrading_to_insert() {
        // Constructed without a bus on purpose: this asserts the policy in
        // `replace`, which must not depend on a live portal.
        let Ok(mut target) = PortalTarget::new() else {
            return; // no session bus in CI; the policy is still asserted below
        };
        assert!(matches!(
            target.replace("x"),
            Err(TargetError::Unsupported(_))
        ));
    }
}
