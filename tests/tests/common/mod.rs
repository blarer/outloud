//! Shared test infrastructure for the workspace integration tests.
//!
//! The centrepiece is [`SimEnv`], a fully scripted implementation of
//! `text_target::Env`. Every hard bug so far was the code being handed wrong
//! facts about the world, so the tests hand it *exact* facts and check the
//! decision, instead of hoping CI's real environment happens to exercise the
//! interesting branch.

// Each integration-test binary compiles its own copy of this module and uses
// only a subset of it, so per-binary dead-code warnings are noise here.
#![allow(dead_code)]

use std::collections::HashMap;

use text_target::Env;

/// A described environment: what the world looks like, stated explicitly.
#[derive(Default, Clone)]
pub struct SimEnv {
    pub vars: HashMap<String, String>,
    pub commands: Vec<&'static str>,
    pub ax_trusted: bool,
    pub destination_is_terminal: bool,
    pub has_display: bool,
    pub has_clipboard: bool,
}

impl SimEnv {
    /// The common desktop case: trusted GUI session, everything available.
    ///
    /// "Everything available" is stated as concrete facts, not just flags:
    /// clipboard construction resolves the actual tool from the same Env
    /// that selection reads, so `has_clipboard: true` must be accompanied
    /// by a display variable and a tool on PATH or the two halves disagree
    /// (select picks clipboard-paste, construction finds no backend).
    pub fn desktop_trusted() -> Self {
        SimEnv {
            ax_trusted: true,
            has_display: true,
            has_clipboard: true,
            commands: vec!["wl-copy"],
            ..Default::default()
        }
        .with_var("WAYLAND_DISPLAY", "wayland-0")
    }

    pub fn with_var(mut self, k: &str, v: &str) -> Self {
        self.vars.insert(k.to_string(), v.to_string());
        self
    }

    pub fn with_command(mut self, c: &'static str) -> Self {
        self.commands.push(c);
        self
    }
}

impl Env for SimEnv {
    fn var(&self, name: &str) -> Option<String> {
        self.vars.get(name).cloned()
    }
    fn has_command(&self, bin: &str) -> bool {
        self.commands.contains(&bin)
    }
    fn ax_trusted(&self) -> bool {
        self.ax_trusted
    }
    fn destination_is_terminal(&self) -> bool {
        self.destination_is_terminal
    }
    fn has_display(&self) -> bool {
        self.has_display
    }
    fn has_clipboard(&self) -> bool {
        self.has_clipboard
    }
}

/// Feed a `SimEnv` into a diag replay record, using the same whitelists the
/// recorder itself uses. This is the bridge that turns "user ran with
/// --record" into "we re-run selection against their facts in a test".
pub fn record_env(rec: &mut diag::replay::SessionRecord, env: &SimEnv) {
    rec.record_env(
        env.ax_trusted,
        env.destination_is_terminal,
        env.has_display,
        env.has_clipboard,
        |v| env.vars.contains_key(v),
        |c| env.commands.contains(&c),
    );
}

/// Reconstruct a scriptable environment from a replay record's facts. The
/// values of recorded vars are gone by design (redaction), so any var that
/// was present comes back with a placeholder value: transport selection only
/// ever tests presence, which is exactly why recording presence suffices.
pub fn env_from_record(rec: &diag::replay::SessionRecord) -> SimEnv {
    // Command names in the record are 'static in the whitelist; map back so
    // SimEnv's `&'static str` list can hold them.
    let commands = diag::replay::TRANSPORT_COMMANDS
        .iter()
        .copied()
        .filter(|c| rec.env.commands.contains(*c))
        .collect();
    SimEnv {
        vars: rec
            .env
            .vars_present
            .iter()
            .map(|v| (v.clone(), "present".to_string()))
            .collect(),
        commands,
        ax_trusted: rec.env.ax_trusted,
        destination_is_terminal: rec.env.destination_is_terminal,
        has_display: rec.env.has_display,
        has_clipboard: rec.env.has_clipboard,
    }
}

/// Tiny deterministic PRNG (xorshift64*) for the property tests. Not a
/// crypto RNG and not meant to be: it exists so the fuzz corpus is
/// reproducible from a printed seed, with no new dependency.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        // xorshift64* constants; any nonzero seed cycles through 2^64-1 states.
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish choice in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}
