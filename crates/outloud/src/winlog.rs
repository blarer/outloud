//! Windows stdio plumbing for a GUI-subsystem daemon.
//!
//! Release builds carry `windows_subsystem = "windows"` (no console window
//! on double-click, see main.rs). That kills the only surface Windows
//! errors had: with no console, `GetStdHandle` returns NULL and Rust's
//! stdio silently DISCARDS every `println!`/`eprintln!` (std special-cases
//! the NULL handle as "write succeeded, wrote nothing" -- lost, not
//! crashed). A daemon whose every error vanishes is strictly worse than an
//! ugly console box, so this module runs before any output and points the
//! std handles somewhere real:
//!
//! 1. Std handles already valid (console-subsystem debug build, or output
//!    redirected by the launching shell: `outloud --version > v.txt`):
//!    touch nothing.
//! 2. `AttachConsole(ATTACH_PARENT_PROCESS)` succeeds (GUI build launched
//!    FROM a shell: `outloud --permissions` in cmd): bind stdout/stderr to
//!    that console's `CONOUT$`, so CLI flags still print where the user
//!    typed. The shell's prompt may interleave with our output -- cmd does
//!    not wait for GUI-subsystem processes -- which is standard Windows
//!    behaviour for GUI apps with CLI flags, not a bug here.
//! 3. Neither (the double-click launch): append both handles to
//!    `outloud.log` beside `config.toml`, so "it just vanished" becomes a
//!    file with the error in it. The panic message lands there too: the
//!    default panic hook writes to stderr, and stderr IS the log now.
//!
//! Called from the top of `main()` on Windows. A no-op everywhere else so
//! the call site needs no cfg.

/// Where daemon output goes when there is no console: beside the config
/// file, which is where `run_diagnostics` already writes `diagnostics.txt`,
/// so a bug report gathers everything from one folder.
#[cfg(windows)]
pub fn log_path() -> Option<std::path::PathBuf> {
    Some(config::user_config_path()?.parent()?.join("outloud.log"))
}

/// Make `println!`/`eprintln!` land somewhere a human can find.
///
/// Must run before the process writes ANY output: Rust queries
/// `GetStdHandle` per write, so handles swapped here are picked up by all
/// later writes, but anything printed earlier followed the old handles.
#[cfg(windows)]
pub fn init() {
    use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_ALWAYS, OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_OUTPUT_HANDLE,
    };

    unsafe {
        // Case 1: output already goes somewhere (own console, or the shell
        // redirected it). NULL means "no console and not redirected";
        // anything else is a live handle that must not be stolen.
        let err = GetStdHandle(STD_ERROR_HANDLE).unwrap_or(INVALID_HANDLE_VALUE);
        if !err.is_invalid() && err != HANDLE(std::ptr::null_mut()) {
            return;
        }

        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };

        // Case 2: a parent shell exists; print into it.
        if AttachConsole(ATTACH_PARENT_PROCESS).is_ok() {
            let name = wide("CONOUT$");
            if let Ok(h) = CreateFileW(
                windows::core::PCWSTR(name.as_ptr()),
                FILE_GENERIC_WRITE.0,
                FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            ) {
                let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
                let _ = SetStdHandle(STD_ERROR_HANDLE, h);
            }
            return;
        }

        // Case 3: double-clicked. Log file, append mode.
        let Some(path) = log_path() else { return };
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
        let name = wide(&path.to_string_lossy());
        if let Ok(h) = CreateFileW(
            windows::core::PCWSTR(name.as_ptr()),
            // FILE_APPEND_DATA alone would be the textbook append handle,
            // but Rust's stdio uses WriteFile which honours the append-only
            // access mode only when GENERIC_WRITE is absent; this is the
            // combination that both appends and accepts WriteFile.
            windows::Win32::Storage::FileSystem::FILE_APPEND_DATA.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ) {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
            let _ = SetStdHandle(STD_ERROR_HANDLE, h);
            // One line per launch so interleaved runs in the log have
            // boundaries. SystemTime: no chrono dependency for a timestamp.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            eprintln!("--- outloud launch (unix {now}) ---");
        }
        // CreateFileW failed (exotic ACL, disk full): handles stay NULL and
        // Rust keeps silently discarding output, which is the least-bad
        // remaining option -- the daemon must still run.
    }
}

#[cfg(not(windows))]
pub fn init() {}

#[cfg(all(test, windows))]
mod tests {
    /// The log lands beside config.toml, where diagnostics.txt already
    /// goes: one folder to attach to a bug report.
    #[test]
    fn log_path_is_beside_the_config_file() {
        let log = super::log_path().expect("a home exists on the test machine");
        let config = config::user_config_path().unwrap();
        assert_eq!(log.parent(), config.parent());
        assert_eq!(log.file_name().unwrap(), "outloud.log");
    }
}
