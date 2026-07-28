//! Visual demo: shows the overlay cycling through every visible state with
//! a synthetic audio level and a growing partial-text tail, so a human can
//! verify with their own eyes that it renders, floats over other windows,
//! follows Spaces, and never steals keyboard focus (type into another app
//! while it runs — your keystrokes must keep landing there).
//!
//! Run: `cargo run -p overlay --bin overlay-demo`
//! It exits on its own after two full cycles (~40s), or Ctrl-C it.

fn main() -> anyhow::Result<()> {
    #[cfg(all(target_os = "macos", feature = "display"))]
    return demo::run();

    #[cfg(all(target_os = "windows", feature = "display"))]
    return windemo::run();

    #[cfg(not(any(
        all(target_os = "macos", feature = "display"),
        all(target_os = "windows", feature = "display")
    )))]
    {
        // Same contract as `overlay::platform_overlay()`: unsupported is a
        // clean, explained exit, not a crash — this binary must compile and
        // run everywhere the workspace builds.
        eprintln!(
            "overlay-demo: unsupported here (needs macOS or Windows and the `display` \
             feature). The state machine itself is platform-neutral; see \
             `overlay::OverlayState`."
        );
        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "display"))]
mod demo {
    use std::cell::RefCell;
    use std::time::Instant;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};
    use overlay::{Anchor, Overlay, OverlayFrame, OverlayState, Point};

    /// The script: every state in launch order with representative data,
    /// including the three invisible states so the "hides itself" half of
    /// the contract is also demonstrated.
    fn script() -> Vec<(OverlayState, Option<&'static str>)> {
        vec![
            (
                OverlayState::ModelLoading,
                Some("Loading model… will transcribe in ~3s"),
            ),
            (OverlayState::Idle, None), // must disappear
            (OverlayState::Listening, None),
            (OverlayState::Transcribing, Some("… 612ms")),
            (OverlayState::Injecting, None), // must disappear (~13-47ms in real life)
            (
                OverlayState::Error,
                Some("Field refused write → text on clipboard, press ⌘V"),
            ),
            (
                OverlayState::NoPermission,
                Some("Accessibility revoked → Re-grant…"),
            ),
            (OverlayState::DegradedOffline, None), // must disappear
        ]
    }

    const PARTIAL: &str =
        "change the deploy target from staging to production and rerun the smoke tests";
    /// Seconds per state. Long enough to see, short enough to cycle.
    const STEP_SECS: f64 = 2.5;
    const CYCLES: usize = 2;

    pub fn run() -> anyhow::Result<()> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("demo must run on the main thread"))?;
        // Accessory policy: no Dock icon, no menu bar takeover. The demo
        // must not activate itself — the entire point is to prove the panel
        // shows while some *other* app keeps focus.
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let mut ov = overlay::platform_overlay()?;
        let start = Instant::now();
        let states = script();
        let total = states.len() * CYCLES;
        let overlay_cell = RefCell::new(ov.as_mut() as *mut dyn Overlay);
        let prev_step = RefCell::new(usize::MAX);

        println!(
            "overlay-demo: cycling {} states x{} ({}s each). Focus another app and type; \
             your keystrokes must keep landing there.",
            states.len(),
            CYCLES,
            STEP_SECS
        );

        // A repeating NSTimer on the main run loop drives frames at ~30 Hz.
        // The demo never spawns threads: everything AppKit stays on main.
        let tick = RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
            let elapsed = start.elapsed().as_secs_f64();
            let step = (elapsed / STEP_SECS) as usize;
            if step >= total {
                unsafe {
                    let mtm = MainThreadMarker::new_unchecked();
                    NSApplication::sharedApplication(mtm).terminate(None);
                }
                return;
            }
            let (state, detail) = states[step % states.len()];
            if prev_step.replace(step) != step {
                println!("  [{:>2}/{}] {}", step + 1, total, state);
            }
            let within = (elapsed % STEP_SECS) / STEP_SECS;
            // Synthetic mic level: a speech-like envelope.
            let level = ((elapsed * 6.0).sin() * 0.5 + 0.5) * 0.7 + 0.1;
            // The tail grows through the step, like live recognition.
            let shown = (PARTIAL.len() as f64 * within) as usize;
            let frame = OverlayFrame {
                state,
                audio_level: level as f32,
                partial_text: PARTIAL[..shown.min(PARTIAL.len())].to_string(),
                detail: detail.map(str::to_string),
                // Anchor near a fake caret that drifts, demonstrating the
                // caret-following placement; a real host passes
                // AXBoundsForRange from ax-edit here.
                anchor: Anchor::Cursor(Point {
                    x: 500.0 + (step as f64 * 40.0),
                    y: 300.0 + (elapsed * 3.0) % 60.0,
                }),
            };
            unsafe {
                let ov = &mut **overlay_cell.borrow_mut();
                let _ = ov.render(&frame);
            }
        });

        unsafe {
            let timer = NSTimer::timerWithTimeInterval_repeats_block(1.0 / 30.0, true, &tick);
            // Common modes so redraws continue during window drags etc.
            let run_loop = NSRunLoop::currentRunLoop();
            run_loop.addTimer_forMode(&timer, NSRunLoopCommonModes);
        }
        app.run();
        // NSApplication::run never returns after terminate; keep the
        // overlay alive until then.
        let _keep: Retained<NSApplication> = app;
        Ok(())
    }
}

/// The same demo for Windows. Kept separate rather than abstracted over the
/// two platforms because the driving loops differ in kind: AppKit needs its
/// run loop pumped from an NSTimer block, while the layered window needs
/// nothing but a sleep. Sharing the script is what matters, and it does.
#[cfg(all(target_os = "windows", feature = "display"))]
mod windemo {
    use std::time::Instant;

    use overlay::{Anchor, OverlayFrame, OverlayState, Point};

    const PARTIAL: &str =
        "change the deploy target from staging to production and rerun the smoke tests";
    const STEP_SECS: f64 = 2.5;
    const CYCLES: usize = 2;

    /// Same script as the macOS demo, with the paste hint spelled for
    /// Windows keyboards. The invisible states are included deliberately:
    /// half the contract is that the overlay HIDES itself.
    fn script() -> Vec<(OverlayState, Option<&'static str>)> {
        vec![
            (
                OverlayState::ModelLoading,
                Some("Loading model... will transcribe in ~3s"),
            ),
            (OverlayState::Idle, None),
            (OverlayState::Listening, None),
            (OverlayState::Transcribing, Some("... 612ms")),
            (OverlayState::Injecting, None),
            (
                OverlayState::Error,
                Some("Field refused write -> text on clipboard, press Ctrl+V"),
            ),
            (
                OverlayState::NoPermission,
                Some("Focused window is elevated (UIPI) -> switch windows"),
            ),
            (OverlayState::DegradedOffline, None),
        ]
    }

    pub fn run() -> anyhow::Result<()> {
        let mut ov = overlay::platform_overlay()?;
        let states = script();
        let total = states.len() * CYCLES;
        let start = Instant::now();

        println!(
            "overlay-demo: cycling {} states x{} ({}s each). Focus another app and type; \
             your keystrokes must keep landing there, and clicks must pass through the \
             overlay to whatever is underneath.",
            states.len(),
            CYCLES,
            STEP_SECS
        );

        let mut prev_step = usize::MAX;
        loop {
            let elapsed = start.elapsed().as_secs_f64();
            let step = (elapsed / STEP_SECS) as usize;
            if step >= total {
                ov.hide()?;
                return Ok(());
            }
            let (state, detail) = states[step % states.len()];
            if prev_step != step {
                println!("  [{:>2}/{}] {}", step + 1, total, state);
                prev_step = step;
            }
            let within = (elapsed % STEP_SECS) / STEP_SECS;
            // A synthetic meter and a growing tail, so both live elements
            // are visibly exercised rather than drawn once and frozen.
            let frame = OverlayFrame {
                state,
                audio_level: ((elapsed * 3.0).sin() * 0.5 + 0.5) as f32,
                partial_text: PARTIAL
                    .chars()
                    .take((PARTIAL.chars().count() as f64 * within) as usize)
                    .collect(),
                detail: detail.map(str::to_string),
                anchor: Anchor::Cursor(Point { x: 600.0, y: 400.0 }),
            };
            ov.render(&frame)?;
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
    }
}
