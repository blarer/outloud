//! Launch-at-login: make `launch-at-login = true` actually start the daemon.
//!
//! Windows only, for now. The mechanism is the per-user Run key
//! (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`): no admin rights,
//! no Task Scheduler XML, visible and deletable in Task Manager's Startup
//! tab, and removed cleanly by deleting one value. A Startup-folder .lnk was
//! the other candidate; the registry value wins because writing a .lnk needs
//! COM (IShellLink) for a correct link, while the Run key is one API call
//! and survives the exe moving only as well as a shortcut would anyway.
//!
//! Called from `MenuHost::reload()` rather than from a menu click handler,
//! so every way of setting the key behaves identically: the menu toggle, a
//! hand edit to config.toml, and an `OUTLOUD_LAUNCH_AT_LOGIN` env override
//! all converge on the same sync. The sync is idempotent and compares
//! before writing, so a daemon that reloads config every few seconds does
//! not hammer the registry.
//!
//! The registered command always points at THIS exe (`current_exe`), which
//! is also what repairs a stale entry after the binary moves: the next
//! launch from the new location rewrites the value.

/// The Run-key value name. A constant because the uninstall path (sync with
/// `false`) must delete exactly what the install path wrote.
pub const RUN_VALUE_NAME: &str = "OutLoud";

/// The command line to register: the exe path, quoted.
///
/// Quoted unconditionally, not only when the path contains a space:
/// `C:\Program Files\...` is the common case, and an unquoted path with a
/// space in the Run key is the classic planted-binary vulnerability
/// (Windows would try `C:\Program.exe` first).
///
/// Pure function of the path so the quoting rule is testable on every
/// platform, not just where the registry exists.
pub fn run_command(exe: &std::path::Path) -> String {
    format!("\"{}\"", exe.display())
}

/// Bring the Run key in line with the configured value. Returns a
/// user-facing problem string on failure rather than an `Err`, matching how
/// `MenuHost::reload()` reports everything else: a daemon must never die
/// over a settings write, and the menu is where the user will look.
#[cfg(windows)]
pub fn sync(enabled: bool) -> Option<String> {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return Some(format!("launch-at-login: cannot locate this exe: {e}")),
    };
    let desired = enabled.then(|| run_command(&exe));
    match registry::sync_run_value(RUN_VALUE_NAME, desired.as_deref()) {
        Ok(()) => None,
        Err(e) => Some(format!("launch-at-login: {e}")),
    }
}

/// Everywhere else the key stays schema-valid but inert, and the docs say
/// so. Returning `None` (no problem line) rather than a "not implemented"
/// warning is deliberate: `Config::inert_settings()` already reports unwired
/// keys once, and two surfaces nagging about the same line is noise.
#[cfg(not(windows))]
pub fn sync(_enabled: bool) -> Option<String> {
    None
}

/// What the Run key currently says for our value, for diagnostics and
/// verification. `None` means no entry.
#[cfg(windows)]
pub fn current_entry() -> Option<String> {
    registry::read_run_value(RUN_VALUE_NAME)
}

#[cfg(windows)]
mod registry {
    //! Minimal HKCU\...\Run access via the windows crate's raw Win32
    //! registry API. Raw rather than a registry-wrapper crate because the
    //! whole surface needed is read / set-string / delete on one well-known
    //! key, and the workspace already depends on `windows`.

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
        REG_VALUE_TYPE,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Open (creating if absent) the per-user Run key. The key exists on
    /// every stock Windows install, but `RegCreateKeyExW` rather than
    /// `RegOpenKeyExW` costs nothing and removes the one failure mode.
    fn open_run_key(access: u32) -> Result<OwnedKey, String> {
        let mut hkey = HKEY::default();
        let path = wide(RUN_KEY);
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                None,
                None,
                REG_OPTION_NON_VOLATILE,
                windows::Win32::System::Registry::REG_SAM_FLAGS(access),
                None,
                &mut hkey,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("cannot open HKCU\\{RUN_KEY} (status {})", status.0));
        }
        Ok(OwnedKey(hkey))
    }

    /// RAII so an early return cannot leak the handle.
    struct OwnedKey(HKEY);
    impl Drop for OwnedKey {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    pub fn read_run_value(name: &str) -> Option<String> {
        let key = open_run_key(KEY_QUERY_VALUE.0).ok()?;
        let name_w = wide(name);
        let mut kind = REG_VALUE_TYPE::default();
        let mut len: u32 = 0;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name_w.as_ptr()),
                None,
                Some(&mut kind),
                None,
                Some(&mut len),
            )
        };
        if status != ERROR_SUCCESS || kind != REG_SZ {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let mut len2 = len;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name_w.as_ptr()),
                None,
                None,
                Some(buf.as_mut_ptr()),
                Some(&mut len2),
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let units: Vec<u16> = buf[..len2 as usize]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        Some(String::from_utf16_lossy(&units))
    }

    /// Make the named Run value equal `desired` (`None` = absent).
    /// Compare-before-write keeps the registry mtime (and any audit tooling
    /// watching the Run key) quiet across the daemon's routine reloads.
    pub fn sync_run_value(name: &str, desired: Option<&str>) -> Result<(), String> {
        if read_run_value(name).as_deref() == desired {
            return Ok(());
        }
        let key = open_run_key(KEY_SET_VALUE.0)?;
        let name_w = wide(name);
        match desired {
            Some(cmd) => {
                let data = wide(cmd);
                let bytes: &[u8] = unsafe {
                    std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2)
                };
                let status = unsafe {
                    RegSetValueExW(key.0, PCWSTR(name_w.as_ptr()), None, REG_SZ, Some(bytes))
                };
                if status != ERROR_SUCCESS {
                    return Err(format!("cannot write Run value (status {})", status.0));
                }
            }
            None => {
                let status = unsafe { RegDeleteValueW(key.0, PCWSTR(name_w.as_ptr())) };
                if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
                    return Err(format!("cannot delete Run value (status {})", status.0));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The quoting rule, pinned: unquoted paths with spaces in Run keys are
    /// the classic "C:\Program.exe" planting vulnerability, so the quotes
    /// are unconditional, not space-triggered.
    #[test]
    fn run_command_always_quotes() {
        let plain = std::path::Path::new(r"C:\Tools\outloud.exe");
        assert_eq!(run_command(plain), r#""C:\Tools\outloud.exe""#);
        let spaced = std::path::Path::new(r"C:\Program Files\OutLoud\outloud.exe");
        assert_eq!(
            run_command(spaced),
            r#""C:\Program Files\OutLoud\outloud.exe""#
        );
    }

    /// Round-trip against the real HKCU Run key, cleaned up afterwards.
    /// A throwaway value name so a test crash cannot leave a stale
    /// "OutLoud" autostart on a developer machine.
    #[cfg(windows)]
    #[test]
    fn registry_sync_round_trips() {
        const NAME: &str = "OutLoudTestValue";
        // Ensure a clean slate even after a previously-crashed run.
        registry::sync_run_value(NAME, None).unwrap();
        assert_eq!(registry::read_run_value(NAME), None);

        registry::sync_run_value(NAME, Some(r#""C:\x y\outloud.exe""#)).unwrap();
        assert_eq!(
            registry::read_run_value(NAME).as_deref(),
            Some(r#""C:\x y\outloud.exe""#)
        );

        // Idempotent: writing the same value again must succeed quietly.
        registry::sync_run_value(NAME, Some(r#""C:\x y\outloud.exe""#)).unwrap();

        registry::sync_run_value(NAME, None).unwrap();
        assert_eq!(registry::read_run_value(NAME), None);
        // Deleting an absent value is a no-op, not an error.
        registry::sync_run_value(NAME, None).unwrap();
    }
}
