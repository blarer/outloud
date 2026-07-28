//! `aquad` binary: argument parsing, frontend selection, thread layout.
//!
//! Thread layout on macOS with a display: the MAIN thread runs the AppKit
//! overlay (NSPanel requires it), and the entire tokio pipeline runs on a
//! background thread. Headless / --no-overlay inverts: tokio gets the main
//! thread and state transitions go to stderr. Either way the pipeline code
//! is identical; only the renderer differs, which is what the state-machine
//! doc means by "one machine, many surfaces".
//!
//! Usage:
//!   aquad                       # daemon: hold right-option, speak, release
//!   aquad --chord fn            # different hotkey
//!   aquad --once                # one dictation cycle from the mic, then exit
//!   aquad --once --wav f.wav    # one cycle fed from a file (no mic needed)
//!   aquad --once --say "text"   # synthesize with `say`, then run the cycle
//!   aquad --asr mock            # deterministic recognizer (CI / no helper)
//!   aquad --no-overlay          # log states instead of drawing the panel

use aquad::pipeline::{self, Config};
use aquad::recognize;
use aquad::source;
use aquad::state::Engine;
use asr::Recognizer;
use diag::timing::Recorder;

struct Args {
    once: bool,
    wav: Option<std::path::PathBuf>,
    say: Option<String>,
    asr: String,
    chord: String,
    no_overlay: bool,
    /// Feed file audio at real-time pace instead of as fast as possible.
    realtime: bool,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        once: false,
        wav: None,
        say: None,
        asr: "apple".into(),
        chord: "right-option".into(),
        no_overlay: false,
        realtime: false,
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
            "--chord" => args.chord = val("--chord")?,
            "--no-overlay" => args.no_overlay = true,
            "--realtime" => args.realtime = true,
            "--help" | "-h" => {
                println!(
                    "aquad: hold the hotkey, speak, release, text appears\n\
                     --once           run one dictation cycle and exit\n\
                     --wav FILE       feed FILE instead of the microphone (with --once)\n\
                     --say TEXT       synthesize TEXT with `say` and feed it (with --once)\n\
                     --asr apple|mock recognizer backend (default apple)\n\
                     --chord CHORD    hotkey (default right-option)\n\
                     --no-overlay     log state changes instead of drawing the panel\n\
                     --realtime       pace file audio like live speech"
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

fn make_recognizer_factory(kind: &str) -> anyhow::Result<RecognizerFactory> {
    match kind {
        "mock" => Ok(Box::new(|| {
            Ok(Box::new(asr::backends::mock::MockRecognizer::new()) as _)
        })),
        "apple" => Ok(Box::new(|| {
            let r = asr::backends::apple::AppleRecognizer::new()?;
            Ok(Box::new(r) as _)
        })),
        other => anyhow::bail!("unknown --asr backend {other} (want apple or mock)"),
    }
}

/// Synthesize `text` to a 16kHz WAV with the OS voice, for --say.
fn synthesize(text: &str) -> anyhow::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("aquad-say");
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
    ok(
        std::process::Command::new("afconvert")
            .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
            .arg(&aiff)
            .arg(&wav)
            .status()?,
        "afconvert",
    )?;
    Ok(wav)
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;

    let (engine, shared) = Engine::new();

    // The pipeline future, boxed so both thread layouts can run it.
    let file_samples = match (&args.wav, &args.say) {
        (Some(_), Some(_)) => anyhow::bail!("--wav and --say are mutually exclusive"),
        (Some(p), None) => Some(aquad::wav::load_16k_mono(p)?),
        (None, Some(text)) => {
            let p = synthesize(text)?;
            Some(aquad::wav::load_16k_mono(&p)?)
        }
        (None, None) => None,
    };
    anyhow::ensure!(
        file_samples.is_none() || args.once,
        "--wav/--say only make sense with --once"
    );

    let factory = make_recognizer_factory(&args.asr)?;
    let cfg = Config {
        once: args.once,
        // File-driven runs commit on the synthetic KeyUp; mic-driven --once
        // has nobody holding a key, so the VAD endpoint is the commit.
        auto_endpoint: args.once && file_samples.is_none(),
    };

    let chord = args.chord.clone();
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
            let mut _capture = None;
            match file_samples {
                Some(samples) => {
                    source::spawn_wav(samples, args.realtime, ftx.clone());
                }
                None => {
                    _capture = Some(source::spawn_mic(ftx.clone()));
                    if args.once {
                        // No key to hold: capture starts immediately.
                        let _ = ftx.send(source::FrontendEvent::KeyDown);
                    } else {
                        let parsed: hotkey::Chord = chord
                            .parse()
                            .map_err(|e| anyhow::anyhow!("bad --chord: {e}"))?;
                        match source::spawn_hotkey(parsed, ftx.clone()) {
                            Ok(display) => eprintln!("aquad: hold {display} to dictate"),
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
            let reports = pipeline::run(cfg, engine, frx, arx, feed, rrx, &mut recorder).await?;

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
        let shared = shared;
        std::thread::Builder::new()
            .name("aquad-status-log".into())
            .spawn(move || {
                let mut last = None;
                loop {
                    let f = shared.snapshot();
                    if last != Some(f.state) {
                        match &f.detail {
                            Some(d) => eprintln!("aquad: state {} ({d})", f.state),
                            None => eprintln!("aquad: state {}", f.state),
                        }
                        last = Some(f.state);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(33));
                }
            })?;
        run_pipeline()
    } else {
        overlay_main(shared, run_pipeline)
    }
}

/// macOS + display: overlay on the main thread, pipeline behind it.
#[cfg(all(target_os = "macos", feature = "display"))]
fn overlay_main(
    shared: aquad::state::StatusShared,
    run_pipeline: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
) -> anyhow::Result<()> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};

    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("overlay_main must run on the main thread"))?;
    // Accessory: no Dock icon, no menu bar, and critically no activation,
    // so the user's focused app keeps keyboard focus the entire time.
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let mut ov = overlay::platform_overlay()?;

    // The pipeline runs to completion on its own thread; its exit ends the
    // process. A channel (not a join) so the run loop below stays simple.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
    std::thread::Builder::new()
        .name("aquad-pipeline".into())
        .spawn(move || {
            let _ = done_tx.send(run_pipeline());
        })?;

    // Poll-render at ~30Hz by spinning the run loop in short slices. An
    // NSTimer + block would also work (overlay-demo does); the explicit loop
    // keeps the pipeline-exit check and the render on one thread with no
    // Rust-side shared mutability.
    let run_loop = NSRunLoop::currentRunLoop();
    loop {
        let frame = shared.snapshot();
        // Render errors are logged, never fatal: the overlay is an
        // indicator, and dictation must outlive its cosmetic failures.
        if let Err(e) = ov.render(&frame) {
            eprintln!("aquad: overlay render failed: {e}");
        }
        unsafe {
            let until = NSDate::dateWithTimeIntervalSinceNow(1.0 / 30.0);
            run_loop.runMode_beforeDate(NSDefaultRunLoopMode, &until);
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
    shared: aquad::state::StatusShared,
    run_pipeline: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
) -> anyhow::Result<()> {
    let mut ov = overlay::platform_overlay()?;

    let (done_tx, done_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
    std::thread::Builder::new()
        .name("aquad-pipeline".into())
        .spawn(move || {
            let _ = done_tx.send(run_pipeline());
        })?;

    loop {
        let frame = shared.snapshot();
        // Render errors are logged, never fatal: the overlay is an
        // indicator, and dictation must outlive its cosmetic failures.
        if let Err(e) = ov.render(&frame) {
            eprintln!("aquad: overlay render failed: {e}");
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
    _shared: aquad::state::StatusShared,
    run_pipeline: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
) -> anyhow::Result<()> {
    // Unreachable in practice (main() branches on the same cfg), kept so the
    // symbol exists on every build target.
    run_pipeline()
}
