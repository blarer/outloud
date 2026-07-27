//! Capability probing: pick the best available transport for the current
//! environment.
//!
//! The probe answers two questions in order. First, *where are we*: inside a
//! terminal multiplexer, inside a plain terminal, on a desktop with a
//! display server, or headless. Second, *within that place*, which tier can
//! both read and write, because edit-by-voice degrades to dictation the
//! moment reading is lost.
//!
//! The interesting inversion: **inside a terminal, terminal-native beats
//! accessibility**. The AX tier can technically see a terminal window, but
//! (as M0 measured) terminals expose no writable text field, while tmux or
//! the WezTerm CLI give full read-and-write. Preference order is therefore
//! contextual, not a fixed tier ranking.
//!
//! All environment inspection goes through the [`Env`] trait so the
//! selection logic is tested as a pure function of a described environment,
//! not of whatever machine CI happens to run on.

use crate::targets::ax::AxTarget;
use crate::targets::clipboard::ClipboardTarget;
use crate::targets::terminal::{KittyTarget, Osc52Target, ScreenTarget, TmuxTarget, WezTermTarget};
use crate::{TargetError, TextTarget};

/// The facts detection needs about the world, mockable for tests.
pub trait Env {
    /// Environment variable lookup.
    fn var(&self, name: &str) -> Option<String>;
    /// Whether `bin` resolves on `PATH`.
    fn has_command(&self, bin: &str) -> bool;
    /// Whether the process holds accessibility trust (macOS AX today).
    fn ax_trusted(&self) -> bool;
    /// Whether stdin/stdout are attached to a terminal.
    fn is_tty(&self) -> bool;
    /// Whether a display server is reachable.
    fn has_display(&self) -> bool;
    /// Whether a clipboard tool is usable.
    fn has_clipboard(&self) -> bool;
}

/// The real environment.
pub struct SystemEnv;

impl Env for SystemEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn has_command(&self, bin: &str) -> bool {
        std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
            .unwrap_or(false)
    }

    fn ax_trusted(&self) -> bool {
        AxTarget::available()
    }

    fn is_tty(&self) -> bool {
        // /dev/tty opens iff a controlling terminal exists; cheaper and more
        // truthful than isatty on stdin, which pipes would falsify.
        std::fs::File::open("/dev/tty").is_ok()
    }

    fn has_display(&self) -> bool {
        if cfg!(target_os = "macos") {
            // A macOS SSH session has no window server access even though
            // the machine has one; Aqua session type is the discriminator.
            return std::env::var_os("SSH_CONNECTION").is_none()
                || std::env::var_os("DISPLAY").is_some();
        }
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
    }

    fn has_clipboard(&self) -> bool {
        ClipboardTarget::available()
    }
}

/// What [`detect`] decided and why, for logs and the `matrix` harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// `name()` of the chosen target.
    pub name: &'static str,
    /// Human-readable reason, so a surprising pick is explainable.
    pub reason: &'static str,
}

/// Decide the best transport for `env` without constructing it.
///
/// Split from [`detect`] so the decision is testable: it is the part that
/// can be wrong in interesting ways, while construction is mechanical.
pub fn select(env: &dyn Env) -> Selection {
    let in_terminal = env.is_tty();

    if in_terminal {
        // Multiplexers first: they survive SSH and detach, and give the
        // only reliable read path a terminal has.
        if env.var("TMUX").is_some() && env.has_command("tmux") {
            return Selection {
                name: "tmux",
                reason: "inside tmux: capture-pane reads, paste-buffer writes, works headless",
            };
        }
        if env.var("STY").is_some() && env.has_command("screen") {
            return Selection {
                name: "gnu-screen",
                reason: "inside GNU screen: readbuf/hardcopy give write and read",
            };
        }
        if env.var("WEZTERM_PANE").is_some() && env.has_command("wezterm") {
            return Selection {
                name: "wezterm-cli",
                reason: "WezTerm pane: cli get-text reads, send-text writes",
            };
        }
        if env.var("KITTY_WINDOW_ID").is_some() && env.has_command("kitten") {
            return Selection {
                name: "kitty-remote-control",
                reason: "kitty window: kitten @ reads and writes when remote control is enabled",
            };
        }
        // A bare terminal from the outside is write-only; OSC 52 at least
        // lands text somewhere useful without a display server.
        if !env.has_display() {
            return Selection {
                name: "osc52",
                reason: "terminal without display server: OSC 52 reaches the user's clipboard",
            };
        }
    }

    if env.has_display() && env.ax_trusted() {
        return Selection {
            name: "macos-ax",
            reason: "accessibility trusted: in-place read and write, undo preserved",
        };
    }

    if env.has_display() && env.has_clipboard() {
        return Selection {
            name: "clipboard-paste",
            reason: "no accessibility trust: clipboard paste is the universal fallback",
        };
    }

    // Nothing environmental to attach to: the daemon socket is the
    // integration point, and callers in a pipe use StdioFilterTarget
    // directly since only they know their streams.
    Selection {
        name: "daemon-socket",
        reason: "headless with no terminal transport: clients integrate via the daemon protocol",
    }
}

/// Probe the real environment and construct the best target.
pub fn detect() -> Result<Box<dyn TextTarget>, TargetError> {
    detect_with_env(&SystemEnv)
}

/// [`detect`] against an explicit environment, for embedding and tests.
pub fn detect_with_env(env: &dyn Env) -> Result<Box<dyn TextTarget>, TargetError> {
    let selection = select(env);
    let target: Box<dyn TextTarget> = match selection.name {
        "tmux" => Box::new(TmuxTarget::new(None)),
        "gnu-screen" => Box::new(ScreenTarget),
        "wezterm-cli" => Box::new(WezTermTarget::new(None)),
        "kitty-remote-control" => Box::new(KittyTarget),
        "osc52" => Box::new(Osc52Target::new()),
        "macos-ax" => Box::new(AxTarget),
        "clipboard-paste" => Box::new(ClipboardTarget::new()?),
        "daemon-socket" => Box::new(crate::targets::headless::DaemonTarget::new(
            crate::targets::headless::DaemonTarget::default_socket_path(),
        )),
        other => unreachable!("select() returned unknown target {other}"),
    };
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeEnv {
        vars: HashMap<&'static str, &'static str>,
        commands: Vec<&'static str>,
        ax_trusted: bool,
        is_tty: bool,
        has_display: bool,
        has_clipboard: bool,
    }

    impl Env for FakeEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).map(|v| v.to_string())
        }
        fn has_command(&self, bin: &str) -> bool {
            self.commands.contains(&bin)
        }
        fn ax_trusted(&self) -> bool {
            self.ax_trusted
        }
        fn is_tty(&self) -> bool {
            self.is_tty
        }
        fn has_display(&self) -> bool {
            self.has_display
        }
        fn has_clipboard(&self) -> bool {
            self.has_clipboard
        }
    }

    #[test]
    fn tmux_beats_accessibility_inside_a_terminal() {
        let env = FakeEnv {
            vars: HashMap::from([("TMUX", "/tmp/tmux-501/default,123,0")]),
            commands: vec!["tmux"],
            ax_trusted: true,
            is_tty: true,
            has_display: true,
            ..Default::default()
        };
        assert_eq!(select(&env).name, "tmux");
    }

    #[test]
    fn tmux_env_without_binary_is_not_tmux() {
        // A detached SSH hop can leave $TMUX set with no tmux on PATH.
        let env = FakeEnv {
            vars: HashMap::from([("TMUX", "stale")]),
            ax_trusted: true,
            is_tty: true,
            has_display: true,
            ..Default::default()
        };
        assert_eq!(select(&env).name, "macos-ax");
    }

    #[test]
    fn wezterm_pane_selected_when_no_multiplexer() {
        let env = FakeEnv {
            vars: HashMap::from([("WEZTERM_PANE", "7")]),
            commands: vec!["wezterm"],
            is_tty: true,
            has_display: true,
            has_clipboard: true,
            ..Default::default()
        };
        assert_eq!(select(&env).name, "wezterm-cli");
    }

    #[test]
    fn multiplexer_beats_host_terminal_ipc() {
        // tmux inside WezTerm: tmux owns the pane the user is editing in.
        let env = FakeEnv {
            vars: HashMap::from([("TMUX", "x"), ("WEZTERM_PANE", "7")]),
            commands: vec!["tmux", "wezterm"],
            is_tty: true,
            has_display: true,
            ..Default::default()
        };
        assert_eq!(select(&env).name, "tmux");
    }

    #[test]
    fn ssh_without_display_falls_to_osc52() {
        let env = FakeEnv {
            is_tty: true,
            has_display: false,
            ..Default::default()
        };
        assert_eq!(select(&env).name, "osc52");
    }

    #[test]
    fn desktop_with_trust_uses_accessibility() {
        let env = FakeEnv {
            ax_trusted: true,
            has_display: true,
            ..Default::default()
        };
        assert_eq!(select(&env).name, "macos-ax");
    }

    #[test]
    fn desktop_without_trust_uses_clipboard() {
        let env = FakeEnv {
            has_display: true,
            has_clipboard: true,
            ..Default::default()
        };
        assert_eq!(select(&env).name, "clipboard-paste");
    }

    #[test]
    fn nothing_at_all_lands_on_the_daemon() {
        let env = FakeEnv::default();
        assert_eq!(select(&env).name, "daemon-socket");
    }

    #[test]
    fn every_selection_is_constructible() {
        // detect_with_env must never hit the unreachable arm: exercise each
        // named selection through construction. Clipboard is skipped when
        // the host lacks a tool, since its constructor is honest about that.
        let envs: Vec<FakeEnv> = vec![
            FakeEnv {
                vars: HashMap::from([("TMUX", "x")]),
                commands: vec!["tmux"],
                is_tty: true,
                ..Default::default()
            },
            FakeEnv {
                vars: HashMap::from([("STY", "x")]),
                commands: vec!["screen"],
                is_tty: true,
                ..Default::default()
            },
            FakeEnv {
                vars: HashMap::from([("KITTY_WINDOW_ID", "1")]),
                commands: vec!["kitten"],
                is_tty: true,
                ..Default::default()
            },
            FakeEnv {
                is_tty: true,
                ..Default::default()
            },
            FakeEnv {
                ax_trusted: true,
                has_display: true,
                ..Default::default()
            },
            FakeEnv::default(),
        ];
        for env in envs {
            let selected = select(&env);
            let built = detect_with_env(&env).unwrap();
            assert_eq!(built.name(), selected.name);
        }
    }
}
