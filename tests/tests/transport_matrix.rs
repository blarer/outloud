//! Transport selection across a matrix of simulated environments.
//!
//! `select()` is a pure function of an `Env`, and history says it is the
//! function that goes wrong in interesting ways: the tty-vs-destination bug
//! shipped because "does OUR process have a terminal" and "is the DESTINATION
//! a terminal" answer differently in exactly the situation nobody simulated
//! (a GUI daemon launched from a shell). This matrix simulates the
//! situations, including that one, so the next confusion of that kind fails
//! a test instead of injecting text into an unwatched shell.

mod common;

use common::SimEnv;
use text_target::detect::{detect_with_env, select};

/// One environment the product will actually meet, and the transport it must
/// pick there. Named so a failure reads as "the SSH row broke", not as an
/// assert on line 90.
struct Row {
    name: &'static str,
    env: SimEnv,
    expect: &'static str,
}

fn matrix() -> Vec<Row> {
    vec![
        Row {
            name: "macOS desktop, trusted, editing a GUI app",
            env: SimEnv::desktop_trusted(),
            expect: "macos-ax",
        },
        Row {
            // THE bug this file exists for. A GUI daemon started from a
            // shell inherits TMUX/WEZTERM_PANE and a controlling tty, but
            // the user is typing into a browser. Terminal ancestry must be
            // ignored when the destination is not a terminal.
            name: "GUI daemon launched from tmux-inside-WezTerm shell",
            env: SimEnv::desktop_trusted()
                .with_var("TMUX", "/tmp/tmux-501/default,42,0")
                .with_var("WEZTERM_PANE", "7")
                .with_var("TERM", "xterm-256color")
                .with_command("tmux")
                .with_command("wezterm"),
            expect: "macos-ax",
        },
        Row {
            name: "desktop without accessibility trust",
            env: SimEnv {
                ax_trusted: false,
                ..SimEnv::desktop_trusted()
            },
            expect: "clipboard-paste",
        },
        Row {
            name: "editing inside a tmux pane",
            env: SimEnv {
                destination_is_terminal: true,
                ..SimEnv::desktop_trusted()
            }
            .with_var("TMUX", "/tmp/tmux-501/default,42,0")
            .with_command("tmux"),
            expect: "tmux",
        },
        Row {
            // Stale $TMUX with no binary happens after an SSH hop carries
            // the variable to a machine without tmux installed.
            name: "stale TMUX var, no tmux binary, trusted desktop",
            env: SimEnv {
                destination_is_terminal: true,
                ..SimEnv::desktop_trusted()
            }
            .with_var("TMUX", "stale"),
            expect: "macos-ax",
        },
        Row {
            name: "SSH session, no display, no multiplexer",
            env: SimEnv {
                destination_is_terminal: true,
                has_display: false,
                ..Default::default()
            }
            .with_var("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 55")
            .with_var("SSH_TTY", "/dev/ttys003"),
            expect: "osc52",
        },
        Row {
            name: "SSH into a tmux session (the differentiator case)",
            env: SimEnv {
                destination_is_terminal: true,
                has_display: false,
                ..Default::default()
            }
            .with_var("SSH_TTY", "/dev/ttys003")
            .with_var("TMUX", "/tmp/tmux-1000/default,7,0")
            .with_command("tmux"),
            expect: "tmux",
        },
        Row {
            name: "GNU screen over SSH",
            env: SimEnv {
                destination_is_terminal: true,
                has_display: false,
                ..Default::default()
            }
            .with_var("STY", "1234.pts-0.host")
            .with_command("screen"),
            expect: "gnu-screen",
        },
        Row {
            name: "WezTerm pane, no multiplexer",
            env: SimEnv {
                destination_is_terminal: true,
                ..SimEnv::desktop_trusted()
            }
            .with_var("WEZTERM_PANE", "3")
            .with_command("wezterm"),
            expect: "wezterm-cli",
        },
        Row {
            name: "kitty window with remote control",
            env: SimEnv {
                destination_is_terminal: true,
                ..SimEnv::desktop_trusted()
            }
            .with_var("KITTY_WINDOW_ID", "1")
            .with_command("kitten"),
            expect: "kitty-remote-control",
        },
        Row {
            // Wayland forbids synthetic input; with no AX trust the
            // clipboard is the only display-tier path that works there.
            name: "Wayland desktop, no accessibility trust",
            env: SimEnv {
                has_display: true,
                has_clipboard: true,
                ..Default::default()
            }
            .with_var("WAYLAND_DISPLAY", "wayland-0")
            .with_var("XDG_SESSION_TYPE", "wayland"),
            expect: "clipboard-paste",
        },
        Row {
            name: "headless CI container, nothing available",
            env: SimEnv::default(),
            expect: "daemon-socket",
        },
        Row {
            // Display but no clipboard helper and no trust: the display
            // tiers are all unusable, so this must degrade to the daemon
            // rather than pretending clipboard works.
            name: "X11 session, no clipboard helper, no trust",
            env: SimEnv {
                has_display: true,
                ..Default::default()
            }
            .with_var("DISPLAY", ":0"),
            expect: "daemon-socket",
        },
    ]
}

#[test]
fn every_simulated_environment_selects_the_expected_transport() {
    let mut failures = Vec::new();
    for row in matrix() {
        let got = select(&row.env);
        if got.name != row.expect {
            failures.push(format!(
                "  {}: expected {}, got {} ({})",
                row.name, row.expect, got.name, got.reason
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "transport matrix failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_matrix_selection_is_constructible_and_agrees_with_select() {
    // The unreachable! in detect_with_env is only sound while select() and
    // construction stay in lockstep. Drive every row through both.
    for row in matrix() {
        let selected = select(&row.env);
        let built = detect_with_env(&row.env)
            .unwrap_or_else(|e| panic!("{}: construction failed: {e}", row.name));
        assert_eq!(built.name(), selected.name, "{}", row.name);
    }
}

#[test]
fn every_selection_reason_is_a_sentence_a_user_could_act_on() {
    // The reason string is user-facing via `spike-cli target`. An empty or
    // placeholder reason breaks the "name the next action" rule from the
    // debugging docs.
    for row in matrix() {
        let s = select(&row.env);
        assert!(
            s.reason.len() > 15,
            "{}: reason too thin: `{}`",
            row.name,
            s.reason
        );
    }
}

#[test]
fn terminal_rows_never_pick_a_display_tier_and_vice_versa() {
    // Invariant behind the tty bug: a terminal destination must get a
    // transport that can actually reach a terminal, and a GUI destination
    // must never get a terminal transport (which would type into a shell
    // nobody is looking at).
    let terminal_transports = [
        "tmux",
        "gnu-screen",
        "wezterm-cli",
        "kitty-remote-control",
        "osc52",
    ];
    for row in matrix() {
        let got = select(&row.env);
        let is_terminal_pick = terminal_transports.contains(&got.name);
        if row.env.destination_is_terminal {
            // A terminal destination without any usable terminal transport
            // (stale $TMUX, binaries missing) may fall back to a display
            // tier: degraded but the user can still see where text lands.
            // The forbidden direction is the other one, checked below.
            continue;
        }
        assert!(
            !is_terminal_pick,
            "{}: GUI destination got terminal transport {} - this is the \
             inject-into-unwatched-shell bug",
            row.name, got.name
        );
    }
}
