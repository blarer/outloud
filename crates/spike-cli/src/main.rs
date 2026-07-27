//! M0 spike harness.
//!
//! The point of the M0 milestone is to answer one question with evidence:
//! can we read and rewrite the focused text field, in place, across the real
//! applications people dictate into? This binary is the instrument for that
//! measurement. It does no speech recognition; it isolates the OS integration
//! risk, which the research identified as the hard part.
//!
//! Usage:
//!   spike-cli probe              read the focused field once
//!   spike-cli watch [interval]   poll the focused field, for app-by-app testing
//!   spike-cli replace <text>     rewrite the selection (or field) in place
//!   spike-cli edit <command>     interpret a spoken edit and apply it in place
//!   spike-cli dry-run <command>  interpret an edit against sample text, no AX needed
//!   spike-cli inspect <app>      scan a named application for text fields
//!   spike-cli target             report the transport this environment resolves to
//!   spike-cli matrix             guided pass over the M0 target applications

use std::time::{Duration, Instant};

use ax_edit::{AxError, RewriteStrategy, TextSnapshot};
use edit_intent::{apply as apply_intent, parse as parse_intent, EditIntent};

/// The applications M0 must work in before the milestone is considered met.
/// They are chosen to cover the distinct text-system families: native AppKit,
/// Electron, a browser, a terminal, and a native chat client.
const M0_TARGETS: &[(&str, &str)] = &[
    ("TextEdit", "native AppKit text system, the easy baseline"),
    (
        "Visual Studio Code",
        "Electron, historically the worst case",
    ),
    ("Safari", "web content, contenteditable and form fields"),
    ("Terminal", "terminal emulator, expected read-only"),
    ("Slack", "Electron chat, the highest-traffic real target"),
];

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // A dictation tool always acts on whatever application the user was already
    // in, never on itself. To reproduce that from a terminal, `--after N` waits
    // N seconds so the operator can click into the target application first.
    // Without it, every measurement would be taken against the terminal.
    let delay_secs = take_flag_value(&mut args, "--after").and_then(|v| v.parse::<u64>().ok());
    if let Some(secs) = delay_secs {
        eprintln!("waiting {secs}s: click into the application you want to test");
        std::thread::sleep(Duration::from_secs(secs));
    }

    // macOS attributes an accessibility grant to the *responsible* process, not
    // to whichever binary makes the call. A binary executed straight from a
    // shell inherits the terminal as its responsible process, so it is judged
    // against the terminal's permission rather than its own. Launching the
    // bundle through LaunchServices (`open`) makes the app responsible for
    // itself, which is how the shipping product will always be started.
    //
    // LaunchServices detaches the process from the terminal, so output is
    // mirrored to a log file that the launching script can display.
    let log_target = std::env::var("AQUA_SPIKE_LOG").ok();
    if let Some(path) = &log_target {
        redirect_output_to(path);
    }

    let command = args.first().map(String::as_str).unwrap_or("probe");

    let exit = match command {
        "probe" => cmd_probe(),
        "watch" => cmd_watch(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1000)),
        "replace" => cmd_replace(&args[1..].join(" ")),
        "edit" => cmd_edit(&args[1..].join(" ")),
        "dry-run" => cmd_dry_run(&args[1..].join(" ")),
        "inspect" => cmd_inspect(&args[1..].join(" ")),
        "target" => cmd_target(),
        "matrix" => cmd_matrix(),
        "help" | "--help" | "-h" => {
            print_help();
            0
        }
        other => {
            eprintln!("unknown command: {other}\n");
            print_help();
            2
        }
    };

    // Record the exit status in the log too, since a detached launch discards it.
    if log_target.is_some() {
        println!("__EXIT__{exit}");
    }

    std::process::exit(exit);
}

/// Point stdout and stderr at `path` so a LaunchServices-detached run is still
/// observable. Failure here is not worth aborting over: the run can proceed
/// silently rather than not at all.
fn redirect_output_to(path: &str) {
    use std::os::unix::io::AsRawFd;
    let Ok(file) = std::fs::File::create(path) else {
        return;
    };
    let fd = file.as_raw_fd();
    // SAFETY: `dup2` onto the standard descriptors is the documented way to
    // redirect them, and `file` is kept alive for the process lifetime below.
    unsafe {
        libc_dup2(fd, 1);
        libc_dup2(fd, 2);
    }
    std::mem::forget(file);
}

extern "C" {
    #[link_name = "dup2"]
    fn libc_dup2(src: i32, dst: i32) -> i32;
}

/// Remove `--name value` from the argument list and return the value.
///
/// The flag is stripped so the remaining arguments can still be joined into a
/// command string without the flag leaking into the text to be inserted.
fn take_flag_value(args: &mut Vec<String>, name: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == name)?;
    if index + 1 >= args.len() {
        args.remove(index);
        return None;
    }
    let value = args.remove(index + 1);
    args.remove(index);
    Some(value)
}

fn print_help() {
    println!(
        "spike-cli - M0 accessibility edit-by-voice harness\n\
         \n\
         COMMANDS\n\
         \x20 probe                Read the focused text field once and report it\n\
         \x20 watch [ms]           Poll the focused field every [ms] (default 1000)\n\
         \x20 replace <text>       Replace the selection, or the whole field, with <text>\n\
         \x20 edit <command>       Interpret a spoken edit command and apply it in place\n\
         \x20 dry-run <command>    Interpret an edit command against sample text, no permission needed\n\
         \x20 inspect <app>        Scan a named application for text fields, even if not frontmost\n\
         \x20 target               Report which transport this environment resolves to, and why\n\
         \x20 matrix               Print the guided M0 application test matrix\n\
         \n\
         OPTIONS\n\
         \x20 --after <seconds>    Wait before acting, so you can focus the target application\n\
         \n\
         The binary must be granted Accessibility permission in\n\
         System Settings > Privacy & Security > Accessibility. When run from a\n\
         terminal, grant the permission to the terminal application itself."
    );
}

/// Nudge the user toward granting permission, but never block on it.
///
/// `AXIsProcessTrusted` reports membership of the approved list, which is not
/// the same thing as whether a call will succeed: the system checks the
/// *responsible* process, and the two disagree often enough that treating this
/// as a gate produces false refusals. So the prompt is offered and the command
/// runs anyway, letting the real API produce the real answer.
fn require_trust() -> bool {
    if ax_edit::is_trusted(false) {
        return true;
    }
    eprintln!(
        "note: this process is not in the Accessibility approved list. Prompting, \
         then attempting the call anyway."
    );
    ax_edit::is_trusted(true);
    true
}

fn cmd_probe() -> i32 {
    if !require_trust() {
        return 1;
    }
    let started = Instant::now();
    match ax_edit::snapshot_focused() {
        Ok(snapshot) => {
            report(&snapshot, started.elapsed());
            0
        }
        Err(err) => {
            eprintln!("probe failed: {err}");
            explain(&err);
            1
        }
    }
}

fn cmd_watch(interval_ms: u64) -> i32 {
    if !require_trust() {
        return 1;
    }
    println!(
        "Polling every {interval_ms}ms. Focus a text field in each target app in turn.\n\
         Press Ctrl-C to stop.\n"
    );
    let mut last = String::new();
    loop {
        let started = Instant::now();
        let line = match ax_edit::snapshot_focused() {
            Ok(snapshot) => format!(
                "{:<14} app={:<34} strategy={:<20} chars={:<6} sel={}",
                snapshot.role,
                snapshot.app.as_deref().unwrap_or("?"),
                snapshot.strategy().to_string(),
                snapshot.value.as_deref().map(str::len).unwrap_or(0),
                snapshot
                    .selection
                    .map(|(l, n)| format!("{l}+{n}"))
                    .unwrap_or_else(|| "-".into()),
            ),
            Err(err) => format!("(no text field: {err})"),
        };
        // Only print on change so a long session stays readable.
        if line != last {
            println!(
                "[{:>6.1}ms] {line}",
                started.elapsed().as_secs_f64() * 1000.0
            );
            last = line;
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}

fn cmd_replace(replacement: &str) -> i32 {
    if replacement.is_empty() {
        eprintln!("replace requires text to write");
        return 2;
    }
    if !require_trust() {
        return 1;
    }

    // Snapshot first so the run reports what was actually replaced. This is
    // also the shape the real edit pipeline takes: read, transform, write.
    let before = match ax_edit::snapshot_focused() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("cannot read focused field: {err}");
            explain(&err);
            return 1;
        }
    };

    println!("before: {:?}", truncate(before.edit_target().unwrap_or("")));
    println!(
        "scope:  {}",
        if before.is_selection_edit() {
            "selection"
        } else {
            "whole field"
        }
    );

    let started = Instant::now();
    match ax_edit::replace_focused(replacement) {
        Ok(strategy) => {
            let elapsed = started.elapsed();
            println!("after:  {:?}", truncate(replacement));
            println!(
                "wrote via {strategy} in {:.1}ms",
                elapsed.as_secs_f64() * 1000.0
            );
            // Verify by reading back. A write that reports success but does not
            // land is the failure mode that matters, and it is common in
            // Electron apps, so the harness always checks.
            match ax_edit::snapshot_focused() {
                Ok(after) => {
                    let landed = after
                        .value
                        .as_deref()
                        .map(|v| v.contains(replacement))
                        .unwrap_or(false);
                    println!(
                        "verify: {}",
                        if landed {
                            "PASS"
                        } else {
                            "FAIL (write did not land)"
                        }
                    );
                    if landed {
                        0
                    } else {
                        1
                    }
                }
                Err(err) => {
                    println!("verify: inconclusive ({err})");
                    0
                }
            }
        }
        Err(AxError::NotSettable) => {
            println!(
                "read-only field: real client must fall back to {}",
                RewriteStrategy::ClipboardPaste
            );
            // This is an expected, handled outcome rather than a spike failure.
            0
        }
        Err(err) => {
            eprintln!("replace failed: {err}");
            explain(&err);
            1
        }
    }
}

/// The full edit-by-voice pipeline against the live focused field:
/// read the field, interpret the spoken command, apply it, write it back.
/// This is the end-to-end path the shipping product will take, minus the
/// speech recognition that produces the command string.
fn cmd_edit(utterance: &str) -> i32 {
    if utterance.is_empty() {
        eprintln!("edit requires a spoken command, e.g. `edit change hello to goodbye`");
        return 2;
    }
    if !require_trust() {
        return 1;
    }

    let read_started = Instant::now();
    let snapshot = match ax_edit::snapshot_focused() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("cannot read focused field: {err}");
            explain(&err);
            return 1;
        }
    };
    let read_ms = read_started.elapsed().as_secs_f64() * 1000.0;

    let Some(target) = snapshot.edit_target() else {
        eprintln!("focused field has no text to edit");
        return 1;
    };

    let parse_started = Instant::now();
    let intent = parse_intent(utterance);
    let parse_us = parse_started.elapsed().as_secs_f64() * 1_000_000.0;

    println!("heard:   {utterance:?}");
    println!("intent:  {}", describe(&intent));
    println!(
        "scope:   {}",
        if snapshot.is_selection_edit() {
            "selection"
        } else {
            "whole field"
        }
    );
    println!("before:  {:?}", truncate(target));

    let apply_started = Instant::now();
    let Some(result) = apply_intent(target, &intent) else {
        match intent {
            EditIntent::Freeform { .. } => println!(
                "no deterministic rule matched; the shipping client escalates this to \
                 a local language model"
            ),
            _ => println!("command did not match anything in the field; nothing was changed"),
        }
        return 0;
    };
    let apply_us = apply_started.elapsed().as_secs_f64() * 1_000_000.0;

    println!("after:   {:?}", truncate(&result));

    // When editing a selection, only the selection is rewritten, so write back
    // the transformed selection rather than the whole field.
    let write_started = Instant::now();
    match ax_edit::replace_focused(&result) {
        Ok(strategy) => {
            let write_ms = write_started.elapsed().as_secs_f64() * 1000.0;
            println!(
                "\ntiming:  read {read_ms:.1}ms | parse {parse_us:.0}us | \
                 apply {apply_us:.0}us | write {write_ms:.1}ms"
            );
            println!("wrote via {strategy}");
            0
        }
        Err(AxError::NotSettable) => {
            println!(
                "\nfield is read-only: the shipping client falls back to {}",
                RewriteStrategy::ClipboardPaste
            );
            0
        }
        Err(err) => {
            eprintln!("write failed: {err}");
            explain(&err);
            1
        }
    }
}

/// Exercise the intent parser without touching the accessibility layer.
///
/// This exists so the language half of edit-by-voice can be developed and
/// demonstrated on any machine, including CI, where no permission is available.
fn cmd_dry_run(utterance: &str) -> i32 {
    if utterance.is_empty() {
        eprintln!("dry-run requires a spoken command");
        return 2;
    }
    const SAMPLE: &str = "the quick brown fox jumps over the lazy dog";

    let started = Instant::now();
    let intent = parse_intent(utterance);
    let elapsed = started.elapsed();

    println!("sample:  {SAMPLE:?}");
    println!("heard:   {utterance:?}");
    println!("intent:  {}", describe(&intent));
    match apply_intent(SAMPLE, &intent) {
        Some(result) => println!("result:  {result:?}"),
        None => println!("result:  (needs a language model, or nothing matched)"),
    }
    println!("parsed in {:.0}us", elapsed.as_secs_f64() * 1_000_000.0);
    0
}

/// Render an intent the way it should read in a user-facing log.
fn describe(intent: &EditIntent) -> String {
    match intent {
        EditIntent::Replace { from, to } => format!("replace {from:?} with {to:?}"),
        EditIntent::Delete { text } => format!("delete {text:?}"),
        EditIntent::Append { text } => format!("append {text:?}"),
        EditIntent::Recase(case) => format!("recase to {case:?}"),
        EditIntent::Freeform { instruction } => format!("freeform: {instruction:?}"),
    }
}

/// Walk a named application's accessibility tree and report its text fields.
///
/// The focus-based commands can only describe the application the user is
/// currently in, which makes an application on another Space or behind other
/// windows impossible to check. More importantly, they cannot distinguish
/// "this application exposes nothing editable" from "the user simply had
/// nothing focused" - and that distinction is the entire question the M0
/// matrix is trying to answer.
fn cmd_inspect(app_name: &str) -> i32 {
    if app_name.is_empty() {
        eprintln!("inspect requires an application name, e.g. `inspect Safari`");
        return 2;
    }
    if !require_trust() {
        return 1;
    }

    let started = Instant::now();
    match ax_edit::find_text_fields(app_name, 18) {
        Ok(scan) if scan.fields.is_empty() && scan.windows == 0 => {
            // Zero windows is not evidence about accessibility support. The
            // window server hides windows that live on another Space, so
            // saying "exposes nothing" here would be an unsupported claim.
            println!("{app_name}: no windows visible to the scan");
            println!(
                "the application may be on another Space; this says nothing about \
                 its accessibility support"
            );
            0
        }
        Ok(scan) if scan.fields.is_empty() => {
            println!(
                "{app_name}: {} window(s), no text fields exposed",
                scan.windows
            );
            println!(
                "this application needs the {} fallback",
                RewriteStrategy::ClipboardPaste
            );
            0
        }
        Ok(scan) => {
            let fields = scan.fields;
            println!(
                "{app_name}: {} window(s), {} text field(s), scanned in {:.0}ms\n",
                scan.windows,
                fields.len(),
                started.elapsed().as_secs_f64() * 1000.0
            );
            for field in fields.iter().take(20) {
                let path = field
                    .path
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(".");
                println!(
                    "  {:<12} writable={:<5} path={:<22} {:?}",
                    field.role,
                    field.settable,
                    path,
                    truncate(field.value.as_deref().unwrap_or(""))
                );
            }
            if fields.len() > 20 {
                println!("  ... and {} more", fields.len() - 20);
            }
            let writable = fields.iter().filter(|f| f.settable).count();
            println!(
                "\n{writable} of {} field(s) support in-place rewrite",
                fields.len()
            );
            0
        }
        Err(err) => {
            eprintln!("inspect failed: {err}");
            explain(&err);
            1
        }
    }
}

/// Report which transport this environment resolves to, and why.
///
/// The focus-based commands assume a graphical session and the accessibility
/// tier. Most destinations are not that: a terminal exposes no writable
/// accessibility field, and an SSH session has no display server at all. This
/// command answers "what would actually be used here", which is the first
/// question to ask when a destination misbehaves.
fn cmd_target() -> i32 {
    let env = text_target::detect::SystemEnv;
    let selection = text_target::detect::select(&env);

    println!("transport: {}", selection.name);
    println!("reason:    {}", selection.reason);

    match text_target::detect::detect_with_env(&env) {
        Ok(target) => {
            let caps = target.capabilities();
            println!("tier:      {}", target.tier());
            println!(
                "can read:  {}  (edit-by-voice needs this; without it only dictation works)",
                caps.can_read
            );
            println!("in place:  {}", caps.can_write_in_place);
            println!("keeps undo:{}", caps.preserves_undo);
            println!("headless:  {}", caps.is_headless);
            0
        }
        Err(err) => {
            eprintln!("could not construct the selected transport: {err}");
            1
        }
    }
}

fn cmd_matrix() -> i32 {
    println!("M0 accessibility test matrix\n");
    println!("For each application: open it, focus a text field, type a sentence,");
    println!("select part of it, then run `spike-cli replace \"rewritten\"`.\n");
    for (app, why) in M0_TARGETS {
        println!("  [ ] {app:<20} {why}");
    }
    println!(
        "\nExit criteria: in-place rewrite succeeds in at least the native, browser,\n\
         and Electron rows. A read-only terminal is an acceptable paste-fallback row."
    );
    0
}

fn report(snapshot: &TextSnapshot, elapsed: Duration) {
    println!("role:      {}", snapshot.role);
    println!(
        "app:       {}",
        snapshot.app.as_deref().unwrap_or("unknown")
    );
    println!(
        "value:     {:?}",
        snapshot.value.as_deref().map(truncate).unwrap_or_default()
    );
    println!(
        "selected:  {:?}",
        snapshot
            .selected_text
            .as_deref()
            .map(truncate)
            .unwrap_or_default()
    );
    println!(
        "selection: {}",
        snapshot
            .selection
            .map(|(loc, len)| format!("location {loc}, length {len}"))
            .unwrap_or_else(|| "none".into())
    );
    println!(
        "writable:  value={} selectedText={}",
        snapshot.value_settable, snapshot.selected_text_settable
    );
    println!("strategy:  {}", snapshot.strategy());
    println!("read in    {:.1}ms", elapsed.as_secs_f64() * 1000.0);
}

/// Turn an error into the next action the operator should take.
fn explain(err: &AxError) {
    match err {
        AxError::NotTrusted => eprintln!(
            "hint: grant Accessibility permission to the app running this binary, \
             usually your terminal"
        ),
        AxError::NoFocusedElement => {
            eprintln!("hint: click into a text field before running the command")
        }
        AxError::NoTextValue => eprintln!(
            "hint: the focused element is not a text field; this app will need the \
             clipboard-paste fallback"
        ),
        AxError::NotSettable => eprintln!("hint: field is read-only; use clipboard-paste fallback"),
        AxError::Api(code) => eprintln!("hint: raw AXError {code}; the target app may be busy"),
        AxError::Unsupported => eprintln!("hint: this platform has no accessibility backend yet"),
    }
}

/// Shorten long field contents so a probe of a large document stays readable,
/// while respecting character boundaries.
fn truncate(text: &str) -> String {
    const LIMIT: usize = 120;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let head: String = text.chars().take(LIMIT).collect();
    format!("{head}... ({} chars total)", text.chars().count())
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn take_flag_value_extracts_and_strips() {
        let mut args = vec!["edit".into(), "--after".into(), "3".into(), "hello".into()];
        assert_eq!(
            super::take_flag_value(&mut args, "--after"),
            Some("3".into())
        );
        assert_eq!(args, vec!["edit".to_string(), "hello".to_string()]);
    }

    #[test]
    fn take_flag_value_handles_missing_and_dangling() {
        let mut args = vec!["edit".into(), "hello".into()];
        assert_eq!(super::take_flag_value(&mut args, "--after"), None);
        assert_eq!(args.len(), 2);

        // A trailing flag with no value must not panic or eat an argument.
        let mut dangling = vec!["edit".into(), "--after".into()];
        assert_eq!(super::take_flag_value(&mut dangling, "--after"), None);
        assert_eq!(dangling, vec!["edit".to_string()]);
    }

    #[test]
    fn truncate_leaves_short_text_alone() {
        assert_eq!(truncate("hello"), "hello");
    }

    #[test]
    fn truncate_reports_total_length() {
        let long = "a".repeat(200);
        let out = truncate(&long);
        assert!(out.ends_with("(200 chars total)"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        // Multi-byte characters must not be split, which a byte-slice would do.
        let long = "é".repeat(200);
        let out = truncate(&long);
        assert!(out.starts_with(&"é".repeat(120)));
    }
}
