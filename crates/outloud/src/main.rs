//! `outloud` binary: argument parsing, frontend selection, thread layout.
//!
//! Thread layout on macOS with a display: the MAIN thread runs the AppKit
//! overlay (NSPanel requires it), and the entire tokio pipeline runs on a
//! background thread. Headless / --no-overlay inverts: tokio gets the main
//! thread and state transitions go to stderr. Either way the pipeline code
//! is identical; only the renderer differs, which is what the state-machine
//! doc means by "one machine, many surfaces".
//!
//! Usage:
//!   outloud                       # daemon: hold right-option, speak, release
//!   outloud --chord fn            # different hotkey
//!   outloud --once                # one dictation cycle from the mic, then exit
//!   outloud --once --wav f.wav    # one cycle fed from a file (no mic needed)
//!   outloud --once --say "text"   # synthesize with `say`, then run the cycle
//!   outloud --asr mock            # deterministic recognizer (CI / no helper)
//!   outloud --no-overlay          # log states instead of drawing the panel

use asr::Recognizer;
use diag::timing::Recorder;
use outloud::mic::Mic;
use outloud::pipeline::{self, Config};
use outloud::recognize;
use outloud::source;
use outloud::state::Engine;

struct Args {
    once: bool,
    wav: Option<std::path::PathBuf>,
    say: Option<String>,
    asr: String,
    chord: String,
    /// Whether `--chord` was passed. An explicit flag must beat the config
    /// file (the config crate's own layer order: a per-run override wins),
    /// and "was it passed" is not recoverable from the value alone once the
    /// default and the file agree.
    chord_from_flag: bool,
    no_overlay: bool,
    /// Feed file audio at real-time pace instead of as fast as possible.
    realtime: bool,
    /// `--sensitivity N` (1-100): override `microphone.sensitivity` for this
    /// run. Exists so the threshold can be swept against a recording without
    /// editing the config file between runs.
    sensitivity: Option<u8>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        once: false,
        wav: None,
        say: None,
        asr: "apple".into(),
        chord: "right-option".into(),
        chord_from_flag: false,
        no_overlay: false,
        realtime: false,
        sensitivity: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .ok_or_else(|| anyhow::anyhow!("{name} needs a value"))
        };
        match a.as_str() {
            "--once" => args.once = true,
            "--wav" => args.wav = Some(val("--wav")?.into()),
            "--say" => args.say = Some(val("--say")?),
            "--asr" => args.asr = val("--asr")?,
            "--chord" => {
                args.chord = val("--chord")?;
                args.chord_from_flag = true;
            }
            "--sensitivity" => {
                let raw = val("--sensitivity")?;
                let n: u8 = raw.parse().map_err(|_| {
                    anyhow::anyhow!("--sensitivity wants a number 1-100, got {raw:?}")
                })?;
                anyhow::ensure!(
                    (1..=100).contains(&n),
                    "--sensitivity must be 1-100, got {n}"
                );
                args.sensitivity = Some(n);
            }
            "--no-overlay" => args.no_overlay = true,
            "--realtime" => args.realtime = true,
            // `--permissions`: report THIS bundle's grants and exit.
            //
            // The doctor runs as a separate bundle (OutLoudDoctor.app) and
            // TCC grants are per-bundle, so it reports its own permissions,
            // not the app's. That sent me chasing a missing grant the app
            // actually had. The only authority on whether OutLoud can see
            // the hotkey is OutLoud.
            "--permissions" => {
                println!(
                    "input-monitoring: {}",
                    if hotkey::has_input_monitoring() {
                        "granted"
                    } else {
                        "MISSING (hotkey cannot fire)"
                    }
                );
                println!(
                    "accessibility:    {}",
                    if ax_edit::is_trusted(false) {
                        "granted"
                    } else {
                        "MISSING (text will paste, not insert)"
                    }
                );
                println!(
                    "bundle:           {}",
                    std::env::current_exe()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| "?".into())
                );
                std::process::exit(0);
            }
            "--version" | "-V" => {
                // Beta support: "what version are you on?" must be
                // answerable by a user who has no idea where the bundle's
                // Info.plist lives, or that it has one.
                println!("outloud {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!(
                    "outloud: hold the hotkey, speak, release, text appears\n\
                     --once           run one dictation cycle and exit\n\
                     --wav FILE       feed FILE instead of the microphone (with --once)\n\
                     --say TEXT       synthesize TEXT with `say` and feed it (with --once)\n\
                     --asr apple|mock recognizer backend (default apple)\n\
                     --chord CHORD    hotkey (default right-option)\n\
                     --no-overlay     log state changes instead of drawing the panel\n\
                     --realtime       pace file audio like live speech\n\
                     --version        print the version and exit"
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument {other} (try --help)"),
        }
    }
    Ok(args)
}

/// A recognizer constructor the worker can call repeatedly (startup probe
/// plus once per utterance).
type RecognizerFactory = Box<dyn Fn() -> anyhow::Result<Box<dyn Recognizer>> + Send + Sync>;

fn make_recognizer_factory(kind: &str, sensitivity: u8) -> anyhow::Result<RecognizerFactory> {
    match kind {
        "mock" => Ok(Box::new(move || {
            // Track the segmenter's threshold rather than keeping a fixed
            // one: otherwise the mock re-gates audio the segmenter already
            // accepted, and a sensitivity sweep measures the fixture
            // instead of the product.
            let knee = audio::vad::EnergyVad::from_sensitivity(sensitivity).knee();
            Ok(Box::new(asr::backends::mock::MockRecognizer::new().with_voiced_rms(knee)) as _)
        })),
        "apple" => Ok(Box::new(|| {
            let r = asr::backends::apple::AppleRecognizer::new()?;
            Ok(Box::new(r) as _)
        })),
        other => anyhow::bail!("unknown --asr backend {other} (want apple or mock)"),
    }
}

/// Synthesize `text` to a 16kHz WAV with the OS voice, for --say.
/// Silence prepended to synthesized speech.
///
/// 300ms: comfortably longer than the segmenter's 150ms pre-roll, so the
/// recognizer has a full window of silence before the first phoneme.
const LEAD_IN: std::time::Duration = std::time::Duration::from_millis(300);

/// Prepend `lead` of digital silence to a 16-bit mono WAV, in place.
///
/// Walks the chunk list rather than assuming a 44-byte header: afconvert
/// emits a `FLLR` padding chunk before `data`, so a fixed offset splices
/// silence into the middle of the header and produces a file whose data
/// chunk cannot be found at all.
fn prepend_silence(path: &std::path::Path, lead: std::time::Duration) -> anyhow::Result<()> {
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(
        bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "{} is not a RIFF/WAVE file",
        path.display()
    );

    // Locate `fmt ` for the sample rate and `data` for the splice point.
    let (mut rate, mut data_at, mut data_len) = (0u64, None, 0usize);
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        match id {
            b"fmt " if size >= 16 => {
                rate = u32::from_le_bytes(bytes[pos + 12..pos + 16].try_into().unwrap()) as u64;
            }
            b"data" => {
                data_at = Some(pos + 8);
                data_len = size.min(bytes.len() - pos - 8);
            }
            _ => {}
        }
        // Chunks are word-aligned; an odd size carries a pad byte.
        pos = pos + 8 + size + (size & 1);
    }

    let data_at = data_at.ok_or_else(|| anyhow::anyhow!("{}: no data chunk", path.display()))?;
    anyhow::ensure!(rate > 0, "{}: no usable sample rate", path.display());

    // 2 bytes per sample, mono: the format afconvert was just asked for.
    let quiet = vec![0u8; (rate * lead.as_millis() as u64 / 1000) as usize * 2];

    let mut out = Vec::with_capacity(bytes.len() + quiet.len());
    out.extend_from_slice(&bytes[..data_at]);
    out.extend_from_slice(&quiet);
    out.extend_from_slice(&bytes[data_at..]);

    // Both length fields now understate the file, and a reader that trusts
    // them truncates exactly the audio we added.
    let new_data = (data_len + quiet.len()) as u32;
    out[data_at - 4..data_at].copy_from_slice(&new_data.to_le_bytes());
    let riff_len = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&riff_len.to_le_bytes());

    std::fs::write(path, out)?;
    Ok(())
}

fn synthesize(text: &str) -> anyhow::Result<std::path::PathBuf> {
    // Per-process directory. `--once` runs are deliberately allowed to run
    // concurrently (benchmarks do), and a shared path made two of them fight
    // over the same file: the loser died with `ExtAudioFileCreateWithURL
    // failed (-48)`, which is `dupFNErr` and reads as a corrupt install
    // rather than as two processes colliding.
    let dir = std::env::temp_dir().join(format!("outloud-say-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let aiff = dir.join("utterance.aiff");
    let wav = dir.join("utterance.wav");
    let ok = |st: std::process::ExitStatus, what: &str| {
        anyhow::ensure!(st.success(), "{what} failed");
        Ok(())
    };
    ok(
        std::process::Command::new("say")
            .args(["-o"])
            .arg(&aiff)
            .arg(text)
            .status()?,
        "say",
    )?;
    // Lead-in silence before the speech.
    //
    // `say` starts the first phoneme at sample zero. A recognizer needs a
    // moment of silence to lock on, so without this the first word is
    // unreliable: "add a period at the end" came back as "a period at the
    // end", "At a period at the end", and "" across three consecutive
    // runs. That turns a valid edit command into an unparseable one, and
    // the resulting text then REPLACES the user's selection, so a test
    // harness artifact looks exactly like a parser bug.
    //
    // Real dictation does not have this problem: a human holds the key,
    // then speaks. This makes --say match that shape rather than testing a
    // condition the product never encounters.
    ok(
        std::process::Command::new("afconvert")
            .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
            .arg(&aiff)
            .arg(&wav)
            .status()?,
        "afconvert",
    )?;
    prepend_silence(&wav, LEAD_IN)?;
    Ok(wav)
}

/// Show a startup refusal to a user who has no terminal.
///
/// Only on the GUI path: a shell launch already printed the message, and a
/// second copy in a terminal would be noise. `display dialog` via osascript
/// rather than an AppKit alert because this runs before `NSApplication` is
/// configured, and because the process is about to exit either way.
#[cfg(target_os = "macos")]
fn report_refusal_to_the_user(message: &str) {
    // A terminal-attached launch has already seen it on stderr.
    if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return;
    }
    // Quoting: the message is ours, not user input, but escaping keeps a
    // future edit containing a quote from silently breaking the dialog.
    let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display dialog \"{escaped}\" with title \"OutLoud\" buttons {{\"OK\"}} \
         default button \"OK\" with icon caution"
    );
    // Best effort: failing to show a dialog must not change the exit path.
    let _ = std::process::Command::new("osascript")
        .args(["-e", &script])
        .status();
}

#[cfg(not(target_os = "macos"))]
fn report_refusal_to_the_user(_message: &str) {
    // Other platforms have no equivalent one-liner, and their daemons are
    // started from a shell where stderr is already visible.
}

fn main() -> anyhow::Result<()> {
    let mut args = parse_args()?;

    // Refuse to be the second daemon. Two copies both bind the hotkey and
    // both open the microphone, so one keypress records the user twice and
    // types their words twice into the field they are focused on. Taken
    // before anything is bound or opened, so a refused start has no effect
    // on the daemon that is already running.
    //
    // Skipped for --once, which is a one-shot measurement: it neither binds
    // the hotkey nor stays resident, and benchmarks run several at a time.
    let _instance = if args.once {
        None
    } else {
        let lock = outloud::instance::acquire().map_err(|e| {
            // A bundled launch has no terminal, so this message would go
            // nowhere: double-clicking OutLoud.app while a daemon is running
            // would simply do nothing at all, which reads as "the app is
            // broken" rather than "it is already running". Say it where a
            // GUI user can see it before the error goes to a stderr nobody
            // is attached to.
            report_refusal_to_the_user(&e.to_string());
            anyhow::anyhow!("{e}")
        })?;
        // Now that the lock is ours, no other daemon is running, so any
        // speech helper still alive belongs to a daemon that is gone. One
        // was found on a dev machine eight hours after its parent died,
        // still holding an OS speech session. The trigger was never
        // reproduced, so this kills the class rather than the cause.
        match outloud::instance::reap_stale_helpers() {
            0 => {}
            n => eprintln!("outloud: cleaned up {n} stale speech helper(s) from a previous run"),
        }
        Some(lock)
    };

    let (engine, shared) = Engine::new();

    // What the daemon actually bound and opened, plus the live switches the
    // pipeline reads. Created first because the menu host writes the master
    // switch into it as soon as it loads the config.
    let runtime = outloud::runtime::RuntimeShared::new();

    // The menu bar owns the configuration the user can see and change, so
    // it is also where the daemon learns which hotkey to bind. Skipped for
    // --once, which is a one-shot measurement that should neither create a
    // config file nor read the user's settings.
    let mut menu_host = (!args.once).then(|| outloud::menuhost::MenuHost::new(runtime.clone()));
    if let Some(host) = &menu_host {
        // An explicit --chord is a per-run override and beats the file,
        // matching the config crate's layer order.
        if !args.chord_from_flag {
            args.chord = host.configured_hotkey().to_string();
        }
    }

    // The pipeline future, boxed so both thread layouts can run it.
    let file_samples = match (&args.wav, &args.say) {
        (Some(_), Some(_)) => anyhow::bail!("--wav and --say are mutually exclusive"),
        (Some(p), None) => Some(outloud::wav::load_16k_mono(p)?),
        (None, Some(text)) => {
            let p = synthesize(text)?;
            Some(outloud::wav::load_16k_mono(&p)?)
        }
        (None, None) => None,
    };
    anyhow::ensure!(
        file_samples.is_none() || args.once,
        "--wav/--say only make sense with --once"
    );

    let cfg = Config {
        once: args.once,
        // File-driven runs commit on the synthetic KeyUp; mic-driven --once
        // has nobody holding a key, so the VAD endpoint is the commit.
        auto_endpoint: args.once && file_samples.is_none(),
        // `insertion.mode = "stream"` in config.toml. `--once` keeps the
        // buffered path: it is a measurement mode and its numbers must stay
        // comparable across runs.
        // OUTLOUD_FORCE_STREAM=1 makes the streaming path reachable under
        // `--once`, which otherwise disables it outright (no menu host to
        // read the setting from). Without this the streaming transport
        // decision could not be exercised offline at all, so a guard added
        // to `wants_streaming` was verifiable only by unit test and never in
        // a real app. That gap is how the Discord streaming bypass survived.
        prefer_streaming: std::env::var_os("OUTLOUD_FORCE_STREAM").is_some_and(|v| v == "1")
            || (!args.once && menu_host.as_ref().is_some_and(|h| h.prefer_streaming())),
        // Falls back to the schema default when there is no menu host, which
        // is the `--once` measurement path: same threshold as a real run, so
        // the numbers stay comparable.
        // Flag beats file beats schema default, the config crate's own
        // layer order.
        sensitivity: args
            .sensitivity
            .or_else(|| menu_host.as_ref().map(|h| h.sensitivity()))
            // Read from config even without a menu host, for the same reason
            // as the silence timeout: `--once` is a measurement mode, and it
            // must segment the way a real run does or its numbers describe a
            // configuration nobody uses.
            .unwrap_or_else(outloud::menuhost::MenuHost::sensitivity_from_config),
        // A live view of the same setting, so editing config.toml takes
        // effect at the next key-down instead of the next launch.
        //
        // Skipped when --sensitivity was passed: an explicit flag must not be
        // silently overridden by a file the user did not touch this run.
        live_sensitivity: match (args.sensitivity, &menu_host) {
            (None, Some(_)) => {
                let runtime = runtime.clone();
                Some(std::sync::Arc::new(move || runtime.sensitivity()) as _)
            }
            _ => None,
        },
        // Read from config even without a menu host: `--once` has no menu,
        // and a safety net that cannot be exercised in a test is a safety
        // net nobody has seen work.
        hot_mic_timeout_ms: menu_host
            .as_ref()
            .map(|h| h.silence_timeout_ms())
            .unwrap_or_else(outloud::menuhost::MenuHost::silence_timeout_from_config),
        warm_hold_ms: menu_host.as_ref().map_or(0, |h| h.warm_hold_ms()),
        // Per-app profiles need a live config; `--once` has no menu host
        // and therefore no profiles, which keeps its numbers comparable
        // across runs.
        resolve_for_app: menu_host.as_ref().map(|h| h.app_resolver()),
    };

    // After cfg, so the mock's voiced-window gate can follow the same
    // sensitivity the segmenter uses.
    let factory = make_recognizer_factory(&args.asr, cfg.sensitivity)?;

    let chord = args.chord.clone();
    let runtime_for_pipeline = runtime.clone();
    let run_pipeline = move || -> anyhow::Result<()> {
        // Two worker threads: the select loop plus spawn_blocking headroom.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()?;
        rt.block_on(async move {
            let (ftx, frx) = tokio::sync::mpsc::unbounded_channel();
            let (atx, arx) = tokio::sync::mpsc::unbounded_channel();
            let (rtx, rrx) = tokio::sync::oneshot::channel();
            // Box<dyn Fn> itself implements Fn, so the factory passes
            // straight through.
            let feed = recognize::spawn(factory, atx, rtx);

            // Frontends: file replay replaces BOTH the hotkey and the mic;
            // a mic-driven --once needs no hotkey (VAD endpoints commit);
            // the full daemon binds the hotkey and opens the mic.
            let mut mic = None;
            match file_samples {
                Some(samples) => {
                    source::spawn_wav(samples, args.realtime, ftx.clone());
                }
                None => {
                    // The microphone is NOT opened here. The pipeline opens
                    // it on key-down and closes it on commit, so the system's
                    // recording indicator means "dictating right now" rather
                    // than "this app is running". See crates/outloud/src/mic.rs.
                    mic = Some(Mic::new(ftx.clone(), runtime_for_pipeline.clone()));
                    if args.once {
                        // No key to hold: capture starts immediately.
                        let _ = ftx.send(source::FrontendEvent::KeyDown);
                    } else {
                        let parsed: hotkey::Chord = chord
                            .parse()
                            .map_err(|e| anyhow::anyhow!("bad --chord: {e}"))?;
                        match source::spawn_hotkey(
                            parsed,
                            ftx.clone(),
                            runtime_for_pipeline.clone(),
                        ) {
                            Ok(display) => {
                                // Binding SUCCEEDING is not the same as the
                                // hotkey working. Without Input Monitoring the
                                // event tap installs cleanly and then simply
                                // never receives a key, so the old message
                                // promised a hotkey that could not fire and
                                // the app looked dead or, worse, looked like
                                // the target app was broken.
                                if hotkey::has_input_monitoring() {
                                    eprintln!("outloud: hold {display} to dictate");
                                } else {
                                    eprintln!(
                                        "outloud: WARNING: no Input Monitoring access, so \
                                         {display} will do NOTHING"
                                    );
                                    eprintln!(
                                        "outloud: grant it in System Settings > Privacy & \
                                         Security > Input Monitoring, then restart OutLoud"
                                    );
                                    eprintln!(
                                        "outloud: (this is a different permission from \
                                         Accessibility, and ad-hoc rebuilds void both)"
                                    );
                                }
                            }
                            Err(e) => {
                                // A dead hotkey is a dead product: fail loudly
                                // with the permission fix named, do not run a
                                // daemon that can never hear its key.
                                anyhow::bail!("hotkey bind failed: {e}");
                            }
                        }
                    }
                }
            }

            let mut recorder = Recorder::new();
            let reports = pipeline::run(
                cfg,
                engine,
                pipeline::Channels {
                    frontend: frx,
                    asr_events: arx,
                    feed,
                    ready: rrx,
                    mic,
                },
                &mut recorder,
            )
            .await?;

            // The honest numbers, printed where a script can scrape them.
            for r in &reports {
                println!("{}", r.render());
            }
            for s in recorder.summary() {
                println!("timing: {}", s.render());
            }
            if args.once && reports.is_empty() {
                anyhow::bail!("no utterance was committed (heard nothing?)");
            }
            Ok(())
        })
    };

    // Platforms with a real overlay backend. Everything else logs states to
    // stderr, which is also what --no-overlay forces.
    let has_overlay = cfg!(all(target_os = "macos", feature = "display"))
        || cfg!(all(target_os = "windows", feature = "display"));
    if args.no_overlay || !has_overlay {
        // No GUI: the pipeline owns the main thread; states go to stderr.
        std::thread::Builder::new()
            .name("outloud-status-log".into())
            .spawn(move || {
                let mut last = None;
                loop {
                    let f = shared.snapshot();
                    if last != Some(f.state) {
                        match &f.detail {
                            Some(d) => eprintln!("outloud: state {} ({d})", f.state),
                            None => eprintln!("outloud: state {}", f.state),
                        }
                        last = Some(f.state);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(33));
                }
            })?;
        run_pipeline()
    } else {
        overlay_main(shared, menu_host.take(), runtime, run_pipeline)
    }
}

/// macOS + display: overlay on the main thread, pipeline behind it.
#[cfg(all(target_os = "macos", feature = "display"))]
fn overlay_main(
    shared: outloud::state::StatusShared,
    menu_host: Option<outloud::menuhost::MenuHost>,
    runtime: outloud::runtime::RuntimeShared,
    run_pipeline: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
) -> anyhow::Result<()> {
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSEventMask, NSEventTrackingRunLoopMode,
    };
    use objc2_foundation::NSDate;

    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("overlay_main must run on the main thread"))?;
    // Accessory: no Dock icon, no menu bar, and critically no activation,
    // so the user's focused app keeps keyboard focus the entire time.
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    // Without this the status bar never wires itself up and the menu bar
    // item silently does not appear: AppKit defers that setup until the app
    // considers itself launched, which normally happens inside `run()`. We
    // never call `run()` — the loop below pumps the run loop itself so the
    // pipeline-exit check stays on this thread — so we have to say it.
    app.finishLaunching();

    let mut ov = overlay::platform_overlay()?;

    // The menu bar presence. Created before the pipeline starts so the icon
    // is there from the first instant the app is running: "is it on?" must
    // be answerable during model load, not only once dictation works.
    let mut status_item = match overlay::status_item::MacStatusItem::new(mtm) {
        Ok(item) => Some(item),
        Err(e) => {
            // A missing status item leaves the app invisible but still
            // functional, so it is a loud warning, not a fatal error.
            eprintln!("outloud: could not create the menu bar item: {e}");
            None
        }
    };
    let mut menu_host = menu_host;

    // The pipeline runs to completion on its own thread; its exit ends the
    // process. A channel (not a join) so the run loop below stays simple.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
    std::thread::Builder::new()
        .name("outloud-pipeline".into())
        .spawn(move || {
            let _ = done_tx.send(run_pipeline());
        })?;

    // Poll-render at ~30Hz, pumping AppKit ourselves rather than handing the
    // main thread to `NSApplication::run()`, so the pipeline-exit check and
    // the render stay on one thread with no Rust-side shared mutability.
    //
    // "Pumping" here means DEQUEUEING AND DISPATCHING EVENTS, not just
    // spinning the run loop. Spinning services timers, ports and sources,
    // which is enough to draw; it does not deliver window-server input to
    // AppKit. An earlier version of this loop only spun, and the result was
    // a status item that appeared, updated, and could not be clicked: the
    // mouse-down never reached the status button. It looked fine in a
    // screenshot and it responded to accessibility-driven presses (those
    // call the action directly, bypassing the queue), so only a real click
    // could catch it.
    // Accessibility trust is polled, not observed: macOS offers no
    // notification for a grant changing, and it changes in both directions
    // while the daemon runs. Revocation (TCC reset, re-sign, OS update)
    // otherwise leaves a daemon that believes it is trusted and silently
    // degrades; the grant being ADDED is even more common, because the
    // quickstart tells people to do exactly that with OutLoud already running,
    // and without a re-check they conclude the fix did not work.
    //
    // Once a second, not every frame: `AXIsProcessTrusted` is a cross-process
    // call, and a permission does not change 30 times a second.
    const TRUST_POLL_FRAMES: u32 = 30;
    let mut frames_since_trust_poll = TRUST_POLL_FRAMES; // check immediately

    loop {
        if frames_since_trust_poll >= TRUST_POLL_FRAMES {
            // `false`: never prompt. A prompt on a timer would be a dialog
            // every second, and the menu already offers the deep link.
            runtime.set_accessibility_trusted(ax_edit::is_trusted(false));
            // The tap's OWN permission, which is not Accessibility. Without
            // it the hotkey silently never fires.
            runtime.set_input_monitoring(hotkey::has_input_monitoring());
            frames_since_trust_poll = 0;
        }
        frames_since_trust_poll += 1;

        let frame = shared.snapshot();
        // `overlay.position = "hidden"`: the user asked not to see the
        // floating indicator. The menu bar item still reports every state,
        // so hiding the overlay costs visibility of nothing.
        let show_overlay = menu_host.as_ref().is_none_or(|h| h.overlay_visible());
        let render = if show_overlay {
            // Spectrum bands ride beside the frame: an atomic read per
            // rendered frame from the lock-free meter slot, so an animated
            // backend can drive its jaw without new frames being published.
            ov.set_audio_bands(shared.meter().read().bands);
            ov.render(&frame)
        } else {
            ov.hide()
        };
        // Render errors are logged, never fatal: the overlay is an
        // indicator, and dictation must outlive its cosmetic failures.
        if let Err(e) = render {
            eprintln!("outloud: overlay render failed: {e}");
        }

        // Menu bar: publish the current state, then perform whatever the
        // user clicked since the last tick. Both are cheap; the status item
        // ignores an unchanged model, and clicks are rare.
        if let (Some(item), Some(host)) = (status_item.as_mut(), menu_host.as_mut()) {
            // A hand edit to config.toml must reach the menu too, not just
            // the menu's own writes.
            host.poll_file_changes();
            item.apply(host.model(frame.state, frame.detail.clone(), &runtime.snapshot()));
            for id in item.drain_clicks() {
                if host.handle(id) {
                    // Quit: drop the status item first so the icon leaves
                    // the menu bar immediately rather than lingering until
                    // the process is reaped.
                    let _ = ov.hide();
                    drop(status_item);
                    return Ok(());
                }
            }
        }

        // Block up to one frame for the first event, then drain whatever
        // else is queued without waiting. Blocking is what keeps an idle
        // daemon off the CPU; draining is what keeps a burst of mouse
        // events from taking one frame each.
        //
        // NSEventTrackingRunLoopMode, not NSDefaultRunLoopMode: menu
        // tracking runs in the tracking mode, and asking only for default
        // mode means this loop stops dequeuing the moment a menu opens,
        // freezing the state updates behind it.
        unsafe {
            let deadline = NSDate::dateWithTimeIntervalSinceNow(1.0 / 30.0);
            let mut until: Option<Retained<NSDate>> = Some(deadline);
            while let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                until.as_deref(),
                NSEventTrackingRunLoopMode,
                true,
            ) {
                app.sendEvent(&event);
                // Subsequent iterations must not wait: a nil date polls.
                until = None;
            }
        }
        match done_rx.try_recv() {
            Ok(result) => {
                let _ = ov.hide();
                return result;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                anyhow::bail!("pipeline thread died without reporting")
            }
        }
    }
}

/// Windows + display: the overlay is a layered window that needs no run
/// loop of its own (it paints via UpdateLayeredWindow and takes no input,
/// see crates/overlay/src/windows.rs), so the render loop is a plain
/// 30Hz sleep on the main thread with the pipeline behind it. This mirrors
/// the macOS structure without AppKit's run-loop pumping.
#[cfg(all(target_os = "windows", feature = "display"))]
fn overlay_main(
    shared: outloud::state::StatusShared,
    // No tray backend on Windows yet: the notification-area equivalent
    // (Shell_NotifyIcon) is separate work and belongs to the Windows port.
    _menu_host: Option<outloud::menuhost::MenuHost>,
    _runtime: outloud::runtime::RuntimeShared,
    run_pipeline: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
) -> anyhow::Result<()> {
    let mut ov = overlay::platform_overlay()?;

    let (done_tx, done_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
    std::thread::Builder::new()
        .name("outloud-pipeline".into())
        .spawn(move || {
            let _ = done_tx.send(run_pipeline());
        })?;

    loop {
        let frame = shared.snapshot();
        // Render errors are logged, never fatal: the overlay is an
        // indicator, and dictation must outlive its cosmetic failures.
        if let Err(e) = ov.render(&frame) {
            eprintln!("outloud: overlay render failed: {e}");
        }
        std::thread::sleep(std::time::Duration::from_millis(33));
        match done_rx.try_recv() {
            Ok(result) => {
                let _ = ov.hide();
                return result;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                anyhow::bail!("pipeline thread died without reporting")
            }
        }
    }
}

#[cfg(not(any(
    all(target_os = "macos", feature = "display"),
    all(target_os = "windows", feature = "display")
)))]
fn overlay_main(
    _shared: outloud::state::StatusShared,
    _menu_host: Option<outloud::menuhost::MenuHost>,
    _runtime: outloud::runtime::RuntimeShared,
    run_pipeline: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
) -> anyhow::Result<()> {
    // Unreachable in practice (main() branches on the same cfg), kept so the
    // symbol exists on every build target.
    run_pipeline()
}
