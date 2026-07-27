//! Session recording and deterministic replay.
//!
//! Every hard bug so far reproduced only in one environment: TCC judging the
//! wrong responsible process, windows hiding on another Space, a transport
//! selector asking whether *our* process had a tty instead of whether the
//! *destination* was a terminal. None of those can be debugged from a stack
//! trace, because the code is fine; the *facts the code was given* were wrong.
//!
//! A replay record therefore captures the facts, not the machine: which
//! environment variables existed (names only), which capability probes said
//! yes, which transport was selected and the reason it gave, what shape the
//! focused field had, what the intent and transformation did, and how long
//! each stage took. Given that record, another machine can re-run the
//! decision logic against the recorded facts and see exactly where its answer
//! diverges from what happened on the user's machine. That is the whole
//! trick: the bug class we keep hitting lives in decisions-about-facts, and
//! decisions-about-facts serialize.
//!
//! **Redaction is by construction, not by review.** This tool sees everything
//! the user types, so a record must be safe to paste into a public issue the
//! moment it is produced. The recording methods take the raw values and store
//! only what [`crate::redact`] leaves behind: content becomes a char count,
//! titles become stable hashes, free text is scrubbed of home path and
//! username, environment variables are recorded as *names from a whitelist*,
//! never values (values carry socket paths, session ids, hostnames). There is
//! deliberately no method on [`SessionRecord`] that accepts-and-stores a raw
//! string verbatim.
//!
//! The serialization is a line-oriented `key value` text format rather than
//! JSON: diag has no serde dependency and must not grow one for this (a
//! bug-report artifact wants to be greppable and human-diffable anyway), and
//! one-value-per-line means no escaping rules to get wrong.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

use crate::redact::{redact_content, redact_title, scrub_free_text};
use crate::timing::{Recorder, Stage};
use crate::ErrorClass;

/// First line of every record. Bump the version when the format changes so an
/// old replayer refuses politely instead of misreading fields.
pub const SCHEMA: &str = "aqua-replay v1";

/// Environment variables whose *presence* matters to transport selection.
///
/// A whitelist rather than "everything set": env values leak usernames,
/// socket paths, and hostnames, and even the *names* of arbitrary variables
/// can identify an employer's tooling. Only names, only these.
pub const TRANSPORT_ENV_VARS: &[&str] = &[
    "TMUX",
    "STY",
    "WEZTERM_PANE",
    "KITTY_WINDOW_ID",
    "SSH_CONNECTION",
    "SSH_TTY",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_SESSION_TYPE",
    "TERM",
    "TERM_PROGRAM",
];

/// Commands whose availability matters to transport selection.
pub const TRANSPORT_COMMANDS: &[&str] = &["tmux", "screen", "wezterm", "kitten"];

/// The capability facts transport selection consumes. These mirror
/// `text-target`'s `Env` trait, restated here as plain data because diag must
/// not depend on text-target (it would invert the dependency: text-target's
/// checks already report through diag). The bridge that feeds a
/// [`SessionRecord`] back into the real selector lives with the integration
/// tests, which may depend on both crates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvFacts {
    pub ax_trusted: bool,
    pub destination_is_terminal: bool,
    pub has_display: bool,
    pub has_clipboard: bool,
    /// Names (never values) of whitelisted variables that were set.
    pub vars_present: BTreeSet<String>,
    /// Whitelisted commands that resolved on PATH.
    pub commands: BTreeSet<String>,
}

/// What the transport selector decided, and the reason it printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportRecord {
    pub name: String,
    pub reason: String,
}

/// Shape of the focused field at read time. All content-bearing fields are
/// stored pre-redacted; there is no way to construct this with raw text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxRecord {
    /// AX role, e.g. `AXTextArea`. Roles are framework vocabulary, not user
    /// data, so they are safe verbatim and essential for "wrong tree" bugs.
    pub role: String,
    /// Stable hash of the window title, so two log lines about the same
    /// window correlate without revealing the title.
    pub title: String,
    /// `[redacted: N chars]` for the field contents.
    pub text: String,
    /// Which write strategy the field supports (`set-selected-text`,
    /// `set-value`, `clipboard-paste`).
    pub strategy: String,
}

/// The parsed intent, reduced to its shape. The operand text is what the
/// user said, which is exactly what must never leave the machine; its length
/// still distinguishes "empty needle" bugs from real ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRecord {
    /// `replace` / `delete` / `append` / `recase` / `freeform`.
    pub kind: String,
    pub from_chars: usize,
    pub to_chars: usize,
}

/// What the transformation did, as geometry rather than content.
///
/// The changed window (common-prefix/common-suffix trim) is the heart of the
/// over-edit gate: chars outside it are provably untouched, and a window
/// wider than the intent explains itself as an over-edit without either
/// string being present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransformRecord {
    pub before_chars: usize,
    pub after_chars: usize,
    /// Char offset where before and after first differ.
    pub changed_start: usize,
    /// Chars removed from `before` inside the changed window.
    pub removed_chars: usize,
    /// Chars inserted into `after` inside the changed window.
    pub inserted_chars: usize,
}

/// Outcome of the write-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRecord {
    pub ok: bool,
    /// Strategy used on success, scrubbed error detail on failure.
    pub detail: String,
    /// Failure classification, present on failure. This is what routes the
    /// record: only `bug` belongs in the tracker.
    pub class: Option<ErrorClass>,
}

/// One recorded pipeline run. Build it via the `record_*` methods, ship it
/// via [`SessionRecord::serialize`], resurrect it via [`SessionRecord::parse`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionRecord {
    /// `os arch`, for "only on Intel" style bugs.
    pub platform: String,
    pub env: EnvFacts,
    pub transport: Option<TransportRecord>,
    pub ax: Option<AxRecord>,
    pub intent: Option<IntentRecord>,
    pub transform: Option<TransformRecord>,
    pub write: Option<WriteRecord>,
    /// Stage label -> duration in microseconds. Micros because M0 measured
    /// parse/apply in single-digit micros and millis would round them to 0.
    pub timings: BTreeMap<String, u64>,
}

/// A field where re-running the decision logic against the recorded facts
/// gave a different answer than the original machine got. Divergences are
/// the product of replay: each one localizes a bug to one decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub field: &'static str,
    pub recorded: String,
    pub replayed: String,
}

impl fmt::Display for Divergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: recorded `{}` but replay produced `{}`",
            self.field, self.recorded, self.replayed
        )
    }
}

/// Parse failure. Carries the offending line so a hand-edited or truncated
/// record names its own problem.
#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "replay parse error at line {}: {}",
            self.line, self.message
        )
    }
}

impl std::error::Error for ParseError {}

impl SessionRecord {
    /// Start a record for the current build's platform.
    pub fn new() -> Self {
        SessionRecord {
            platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            ..Default::default()
        }
    }

    /// Capture environment facts. `var_is_set` and `command_exists` are
    /// closures rather than a live probe so the recorder works identically
    /// against the real process environment and a simulated one in tests.
    /// Only whitelisted names are ever consulted, so a caller cannot leak a
    /// value by accident.
    pub fn record_env(
        &mut self,
        ax_trusted: bool,
        destination_is_terminal: bool,
        has_display: bool,
        has_clipboard: bool,
        var_is_set: impl Fn(&str) -> bool,
        command_exists: impl Fn(&str) -> bool,
    ) {
        self.env = EnvFacts {
            ax_trusted,
            destination_is_terminal,
            has_display,
            has_clipboard,
            vars_present: TRANSPORT_ENV_VARS
                .iter()
                .filter(|v| var_is_set(v))
                .map(|v| v.to_string())
                .collect(),
            commands: TRANSPORT_COMMANDS
                .iter()
                .filter(|c| command_exists(c))
                .map(|c| c.to_string())
                .collect(),
        };
    }

    /// Record which transport was selected and why. The reason is scrubbed:
    /// selection reasons are static strings today, but a future dynamic
    /// reason ("socket at /Users/jane/...") must not leak by default.
    pub fn record_transport(&mut self, name: &str, reason: &str) {
        self.transport = Some(TransportRecord {
            name: name.to_string(),
            reason: scrub(reason),
        });
    }

    /// Record the focused field's shape. Title and text arrive raw and are
    /// redacted here, on this side of the API, so no caller can forget.
    pub fn record_ax(&mut self, role: &str, raw_title: &str, raw_text: &str, strategy: &str) {
        self.ax = Some(AxRecord {
            role: role.to_string(),
            title: redact_title(raw_title),
            text: redact_content(raw_text),
            strategy: strategy.to_string(),
        });
    }

    /// Record the intent's shape. Operands arrive raw; only their lengths
    /// are kept, because the operands are the user's transcribed speech.
    pub fn record_intent(&mut self, kind: &str, raw_from: &str, raw_to: &str) {
        self.intent = Some(IntentRecord {
            kind: kind.to_string(),
            from_chars: raw_from.chars().count(),
            to_chars: raw_to.chars().count(),
        });
    }

    /// Record what the transformation did to the text, as geometry. Both
    /// strings arrive raw and neither is stored.
    pub fn record_transform(&mut self, before: &str, after: &str) {
        let (changed_start, removed_chars, inserted_chars) = edit_window(before, after);
        self.transform = Some(TransformRecord {
            before_chars: before.chars().count(),
            after_chars: after.chars().count(),
            changed_start,
            removed_chars,
            inserted_chars,
        });
    }

    pub fn record_write_ok(&mut self, strategy: &str) {
        self.write = Some(WriteRecord {
            ok: true,
            detail: strategy.to_string(),
            class: None,
        });
    }

    /// Record a failed write. Detail is scrubbed because error messages love
    /// to embed paths.
    pub fn record_write_err(&mut self, detail: &str, class: ErrorClass) {
        self.write = Some(WriteRecord {
            ok: false,
            detail: scrub(detail),
            class: Some(class),
        });
    }

    pub fn record_timing(&mut self, stage: &str, elapsed: Duration) {
        self.timings
            .insert(stage.to_string(), elapsed.as_micros() as u64);
    }

    /// Feed the recorded timings into a [`Recorder`] so replayed sessions go
    /// through the same percentile/budget machinery as live ones. Unknown
    /// stage labels land in `Other` rather than being dropped: a replay that
    /// silently loses data is worse than none.
    pub fn timings_recorder(&self) -> Recorder {
        let mut r = Recorder::new();
        for (label, micros) in &self.timings {
            let stage = match label.as_str() {
                "read" => Stage::Read,
                "parse" => Stage::Parse,
                "apply" => Stage::Apply,
                "write" => Stage::Write,
                _ => Stage::Other,
            };
            r.record(stage, Duration::from_micros(*micros));
        }
        r
    }

    /// Compare the recorded transport decision against a freshly replayed
    /// one. This is the check that would have caught the tty-vs-destination
    /// bug: the recorded facts said "destination is not a terminal" while
    /// the recorded selection was a terminal transport, so replaying the
    /// fixed selector against the same facts diverges, and the divergence
    /// *is* the bug report.
    pub fn compare_selection(&self, replayed_name: &str) -> Option<Divergence> {
        let recorded = self.transport.as_ref()?;
        if recorded.name == replayed_name {
            None
        } else {
            Some(Divergence {
                field: "transport",
                recorded: recorded.name.clone(),
                replayed: replayed_name.to_string(),
            })
        }
    }

    /// Internal consistency check, run by the replayer before trusting a
    /// record: a hand-truncated or version-skewed record should be rejected
    /// here, not produce a subtly wrong diagnosis downstream.
    pub fn verify_consistency(&self) -> Result<(), String> {
        if let Some(t) = &self.transform {
            // The changed-window arithmetic must reconstruct the after
            // length exactly; if it does not, the record was not produced by
            // record_transform and its geometry cannot be trusted.
            let derived = t.before_chars - t.removed_chars + t.inserted_chars;
            if derived != t.after_chars {
                return Err(format!(
                    "transform geometry inconsistent: {} - {} + {} != {}",
                    t.before_chars, t.removed_chars, t.inserted_chars, t.after_chars
                ));
            }
            if t.changed_start + t.removed_chars > t.before_chars {
                return Err("changed window exceeds before-text".to_string());
            }
        }
        if let Some(w) = &self.write {
            if !w.ok && w.class.is_none() {
                return Err("failed write must carry an error class".to_string());
            }
        }
        Ok(())
    }

    /// Serialize to the line format. The output is the bug-report artifact:
    /// everything in it has already been redacted at record time.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str(SCHEMA);
        out.push('\n');
        push(&mut out, "platform", &self.platform);
        push(&mut out, "env.ax_trusted", &self.env.ax_trusted.to_string());
        push(
            &mut out,
            "env.destination_is_terminal",
            &self.env.destination_is_terminal.to_string(),
        );
        push(
            &mut out,
            "env.has_display",
            &self.env.has_display.to_string(),
        );
        push(
            &mut out,
            "env.has_clipboard",
            &self.env.has_clipboard.to_string(),
        );
        for v in &self.env.vars_present {
            push(&mut out, "env.var", v);
        }
        for c in &self.env.commands {
            push(&mut out, "env.cmd", c);
        }
        if let Some(t) = &self.transport {
            push(&mut out, "transport.name", &t.name);
            push(&mut out, "transport.reason", &t.reason);
        }
        if let Some(a) = &self.ax {
            push(&mut out, "ax.role", &a.role);
            push(&mut out, "ax.title", &a.title);
            push(&mut out, "ax.text", &a.text);
            push(&mut out, "ax.strategy", &a.strategy);
        }
        if let Some(i) = &self.intent {
            push(&mut out, "intent.kind", &i.kind);
            push(&mut out, "intent.from_chars", &i.from_chars.to_string());
            push(&mut out, "intent.to_chars", &i.to_chars.to_string());
        }
        if let Some(t) = &self.transform {
            push(
                &mut out,
                "transform.before_chars",
                &t.before_chars.to_string(),
            );
            push(
                &mut out,
                "transform.after_chars",
                &t.after_chars.to_string(),
            );
            push(
                &mut out,
                "transform.changed_start",
                &t.changed_start.to_string(),
            );
            push(
                &mut out,
                "transform.removed_chars",
                &t.removed_chars.to_string(),
            );
            push(
                &mut out,
                "transform.inserted_chars",
                &t.inserted_chars.to_string(),
            );
        }
        if let Some(w) = &self.write {
            push(&mut out, "write.ok", &w.ok.to_string());
            push(&mut out, "write.detail", &w.detail);
            if let Some(c) = &w.class {
                push(&mut out, "write.class", &c.to_string());
            }
        }
        for (stage, micros) in &self.timings {
            push(&mut out, "timing", &format!("{stage} {micros}"));
        }
        out
    }

    /// Parse a serialized record. Unknown keys are ignored rather than
    /// rejected so a v1 replayer survives a v1.1 record with extra fields;
    /// the schema line still gates genuinely incompatible formats.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut lines = text.lines().enumerate();
        match lines.next() {
            Some((_, first)) if first.trim() == SCHEMA => {}
            Some((_, first)) => {
                return Err(ParseError {
                    line: 1,
                    message: format!("expected `{SCHEMA}`, got `{first}`"),
                })
            }
            None => {
                return Err(ParseError {
                    line: 1,
                    message: "empty record".to_string(),
                })
            }
        }

        let mut rec = SessionRecord::default();
        // Section structs are assembled field-by-field; only sections that
        // saw at least one key become Some, so a record without an AX read
        // parses back to `ax: None` rather than a struct of empty strings.
        let mut transport: Option<TransportRecord> = None;
        let mut ax: Option<AxRecord> = None;
        let mut intent: Option<IntentRecord> = None;
        let mut transform: Option<TransformRecord> = None;
        let mut write: Option<WriteRecord> = None;

        for (idx, line) in lines {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once(' ').ok_or_else(|| ParseError {
                line: idx + 1,
                message: format!("no value: `{line}`"),
            })?;
            let err = |message: String| ParseError {
                line: idx + 1,
                message,
            };
            let parse_bool = |v: &str| {
                v.parse::<bool>()
                    .map_err(|_| err(format!("expected bool, got `{v}`")))
            };
            let parse_usize = |v: &str| {
                v.parse::<usize>()
                    .map_err(|_| err(format!("expected number, got `{v}`")))
            };
            match key {
                "platform" => rec.platform = value.to_string(),
                "env.ax_trusted" => rec.env.ax_trusted = parse_bool(value)?,
                "env.destination_is_terminal" => {
                    rec.env.destination_is_terminal = parse_bool(value)?
                }
                "env.has_display" => rec.env.has_display = parse_bool(value)?,
                "env.has_clipboard" => rec.env.has_clipboard = parse_bool(value)?,
                "env.var" => {
                    rec.env.vars_present.insert(value.to_string());
                }
                "env.cmd" => {
                    rec.env.commands.insert(value.to_string());
                }
                "transport.name" => section(&mut transport).name = value.to_string(),
                "transport.reason" => section(&mut transport).reason = value.to_string(),
                "ax.role" => section(&mut ax).role = value.to_string(),
                "ax.title" => section(&mut ax).title = value.to_string(),
                "ax.text" => section(&mut ax).text = value.to_string(),
                "ax.strategy" => section(&mut ax).strategy = value.to_string(),
                "intent.kind" => section(&mut intent).kind = value.to_string(),
                "intent.from_chars" => section(&mut intent).from_chars = parse_usize(value)?,
                "intent.to_chars" => section(&mut intent).to_chars = parse_usize(value)?,
                "transform.before_chars" => {
                    section(&mut transform).before_chars = parse_usize(value)?
                }
                "transform.after_chars" => {
                    section(&mut transform).after_chars = parse_usize(value)?
                }
                "transform.changed_start" => {
                    section(&mut transform).changed_start = parse_usize(value)?
                }
                "transform.removed_chars" => {
                    section(&mut transform).removed_chars = parse_usize(value)?
                }
                "transform.inserted_chars" => {
                    section(&mut transform).inserted_chars = parse_usize(value)?
                }
                "write.ok" => section(&mut write).ok = parse_bool(value)?,
                "write.detail" => section(&mut write).detail = value.to_string(),
                "write.class" => {
                    section(&mut write).class = Some(match value {
                        "environment" => ErrorClass::Environment,
                        "permission" => ErrorClass::Permission,
                        "configuration" => ErrorClass::Configuration,
                        "bug" => ErrorClass::Bug,
                        other => return Err(err(format!("unknown error class `{other}`"))),
                    })
                }
                "timing" => {
                    let (stage, micros) = value
                        .split_once(' ')
                        .ok_or_else(|| err(format!("timing needs `stage micros`: `{value}`")))?;
                    let micros = micros
                        .parse::<u64>()
                        .map_err(|_| err(format!("bad micros `{micros}`")))?;
                    rec.timings.insert(stage.to_string(), micros);
                }
                // Forward compatibility: skip, do not fail.
                _ => {}
            }
        }
        rec.transport = transport;
        rec.ax = ax;
        rec.intent = intent;
        rec.transform = transform;
        rec.write = write;
        Ok(rec)
    }
}

/// Get-or-default a section under assembly. Separate fn because the closure
/// form would borrow the option twice in the match arms.
fn section<T: Default>(slot: &mut Option<T>) -> &mut T {
    slot.get_or_insert_with(T::default)
}

// Defaults exist only so `section()` can assemble records field-by-field
// during parsing; recording code never constructs these directly.
impl Default for TransportRecord {
    fn default() -> Self {
        TransportRecord {
            name: String::new(),
            reason: String::new(),
        }
    }
}
impl Default for AxRecord {
    fn default() -> Self {
        AxRecord {
            role: String::new(),
            title: String::new(),
            text: String::new(),
            strategy: String::new(),
        }
    }
}
impl Default for IntentRecord {
    fn default() -> Self {
        IntentRecord {
            kind: String::new(),
            from_chars: 0,
            to_chars: 0,
        }
    }
}
impl Default for TransformRecord {
    fn default() -> Self {
        TransformRecord {
            before_chars: 0,
            after_chars: 0,
            changed_start: 0,
            removed_chars: 0,
            inserted_chars: 0,
        }
    }
}
impl Default for WriteRecord {
    fn default() -> Self {
        WriteRecord {
            ok: false,
            detail: String::new(),
            class: None,
        }
    }
}

fn push(out: &mut String, key: &str, value: &str) {
    // The format is line-framed, so a value containing a newline would smuggle
    // extra keys. Redacted values never contain one, but defend anyway.
    let value = value.replace('\n', " ");
    out.push_str(key);
    out.push(' ');
    out.push_str(&value);
    out.push('\n');
}

/// Scrub free text of home path and username, and flatten newlines so the
/// line format stays framed.
fn scrub(text: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let user = std::env::var("USER").unwrap_or_default();
    scrub_free_text(text, &home, &user).replace('\n', " ")
}

/// Locate the changed window between two strings as char offsets: where they
/// first differ, how many chars of `before` were removed there, how many of
/// `after` were inserted. `(0, 0, 0)` means identical.
///
/// This is what makes the over-edit gate checkable from a redacted record:
/// everything outside the window is provably untouched, by construction of
/// the common prefix and suffix.
pub fn edit_window(before: &str, after: &str) -> (usize, usize, usize) {
    let b: Vec<char> = before.chars().collect();
    let a: Vec<char> = after.chars().collect();
    let prefix = b.iter().zip(a.iter()).take_while(|(x, y)| x == y).count();
    // Suffix must not overlap the prefix, or "aa" -> "a" double-counts the
    // shared char and the removed-count underflows.
    let max_suffix = b.len().min(a.len()) - prefix;
    let suffix = b
        .iter()
        .rev()
        .zip(a.iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    (prefix, b.len() - prefix - suffix, a.len() - prefix - suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_record() -> SessionRecord {
        let mut rec = SessionRecord::new();
        rec.record_env(
            true,
            false,
            true,
            true,
            |v| v == "TERM" || v == "TMUX",
            |c| c == "tmux",
        );
        rec.record_transport("macos-ax", "accessibility trusted: in-place read and write");
        rec.record_ax(
            "AXTextArea",
            "taxes_2025_final.xlsx - Numbers",
            "my secret document text",
            "set-selected-text",
        );
        rec.record_intent("replace", "secret", "public");
        rec.record_transform("my secret document text", "my public document text");
        rec.record_write_ok("set-value");
        rec.record_timing("read", Duration::from_micros(33_500));
        rec.record_timing("parse", Duration::from_micros(7));
        rec
    }

    #[test]
    fn serialize_parse_roundtrip_is_lossless() {
        let rec = full_record();
        let text = rec.serialize();
        let back = SessionRecord::parse(&text).unwrap();
        assert_eq!(rec, back);
        back.verify_consistency().unwrap();
    }

    #[test]
    fn record_never_contains_the_raw_content() {
        // The whole point: recording is redaction. The raw title, field
        // text, and intent operands must be unfindable in the artifact.
        let text = full_record().serialize();
        for secret in ["taxes", "secret document", "public", "xlsx"] {
            assert!(!text.contains(secret), "leaked `{secret}` in:\n{text}");
        }
        // But the diagnostic signal survives.
        assert!(text.contains("23 chars"), "field length lost:\n{text}");
        assert!(text.contains("intent.from_chars 6"));
    }

    #[test]
    fn env_vars_recorded_as_whitelisted_names_only() {
        let mut rec = SessionRecord::new();
        // Claim every var is set: only whitelisted names may appear.
        rec.record_env(false, false, false, false, |_| true, |_| true);
        let text = rec.serialize();
        for v in TRANSPORT_ENV_VARS {
            assert!(text.contains(&format!("env.var {v}")));
        }
        // No values, ever: an env var line is exactly "env.var NAME".
        for line in text.lines().filter(|l| l.starts_with("env.var ")) {
            assert_eq!(line.split(' ').count(), 2, "value smuggled: {line}");
        }
    }

    #[test]
    fn selection_divergence_is_reported() {
        let rec = full_record();
        assert!(rec.compare_selection("macos-ax").is_none());
        let d = rec.compare_selection("tmux").unwrap();
        assert_eq!(d.field, "transport");
        assert!(d.to_string().contains("macos-ax"));
    }

    #[test]
    fn edit_window_finds_the_changed_span() {
        assert_eq!(edit_window("same", "same"), (4, 0, 0));
        assert_eq!(edit_window("the quick fox", "the slow fox"), (4, 5, 4));
        assert_eq!(edit_window("abc", "abXc"), (2, 0, 1));
        // Overlapping repeat: naive suffix matching would underflow here.
        assert_eq!(edit_window("aa", "a"), (1, 1, 0));
        assert_eq!(edit_window("", "hello"), (0, 0, 5));
        // Chars, not bytes: multi-byte prefix must count as chars.
        assert_eq!(edit_window("héllo", "héllx"), (4, 1, 1));
    }

    #[test]
    fn inconsistent_geometry_is_rejected() {
        let mut rec = full_record();
        rec.transform.as_mut().unwrap().after_chars += 1;
        assert!(rec.verify_consistency().is_err());
    }

    #[test]
    fn wrong_schema_and_bad_fields_fail_with_line_numbers() {
        assert!(SessionRecord::parse("").is_err());
        assert!(SessionRecord::parse("some other format\n").is_err());
        let bad = format!("{SCHEMA}\nenv.ax_trusted maybe\n");
        let err = SessionRecord::parse(&bad).unwrap_err();
        assert_eq!(err.line, 2);
    }

    #[test]
    fn unknown_keys_are_skipped_for_forward_compat() {
        let text = format!("{SCHEMA}\nfuture.field hello\nplatform test x\n");
        let rec = SessionRecord::parse(&text).unwrap();
        assert_eq!(rec.platform, "test x");
    }

    #[test]
    fn timings_replay_through_the_recorder() {
        let rec = full_record();
        let recorder = rec.timings_recorder();
        let summary = recorder.summary();
        assert_eq!(summary.len(), 2); // read + parse
        let read = summary
            .iter()
            .find(|s| s.stage == Stage::Read)
            .expect("read stage");
        assert_eq!(read.p50, Duration::from_micros(33_500));
    }
}
