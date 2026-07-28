//! `overlay-proto`: scripted driver for the orb + rolling-window overlay.
//!
//! Design rationale lives in `docs/overlay-redesign.md`. Originally this
//! binary carried its own prototype renderer; that renderer has since been
//! promoted into the shipping overlay (`src/macos.rs`), so today this
//! drives the REAL `MacOverlay` through the same [`overlay::Overlay`] API
//! the daemon uses, feeding it a scripted dictation. It exists so "does the
//! rolling window fade, does committed text turn white, does focus stay
//! elsewhere" is answerable in ~26 seconds without a microphone or a
//! recognizer. Run: `cargo run -p overlay --bin overlay-proto`.

fn main() -> anyhow::Result<()> {
    #[cfg(all(target_os = "macos", feature = "display"))]
    return proto::run();

    #[cfg(not(all(target_os = "macos", feature = "display")))]
    {
        // Same contract as overlay-demo: unsupported platforms get a clean
        // explained exit, because this binary must compile everywhere the
        // workspace builds (including `--no-default-features` headless CI).
        eprintln!(
            "overlay-proto: this driver needs macOS and the `display` feature. \
             The design it demonstrates is documented in docs/overlay-redesign.md."
        );
        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "display"))]
mod proto {
    use std::cell::RefCell;
    use std::time::Instant;

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};
    use overlay::{Anchor, Overlay, OverlayFrame, OverlayState};

    /// (seconds when the word appears in the hypothesis) per word. The
    /// sentence is the product owner's own example extended past the lane
    /// width, so overflow fading MUST trigger: by "has a lot of fun", "the
    /// dog is brown" is fading out.
    const SCRIPT: &[(f64, &str)] = &[
        (2.2, "the"),
        (2.5, "dog"),
        (2.9, "is"),
        (3.2, "brown"),
        (3.7, "and"),
        (4.0, "has"),
        (4.3, "a"),
        (4.5, "lot"),
        (4.8, "of"),
        (5.1, "fun"),
        (5.9, "chasing"),
        (6.4, "the"),
        (6.7, "ball"),
        (7.2, "across"),
        (7.7, "the"),
        (8.0, "park"),
        (8.6, "every"),
        (9.0, "single"),
        (9.5, "morning"),
        (10.2, "before"),
        (10.6, "breakfast"),
    ];
    /// Timeline: model-loading pulse, speech, pause (stale decay drains the
    /// lane), finalize shimmer, exit.
    const T_LISTEN: f64 = 2.0;
    const T_PAUSE: f64 = 11.0;
    const T_TRANSCRIBE: f64 = 19.5;
    const T_END: f64 = 23.5;
    /// Push cadence. Matches the daemon's ~30 Hz poll of the status slot,
    /// so the proto exercises the exact drive rate production uses (the
    /// overlay's own 60 Hz clock does the smoothing, same as production).
    const PUSH_HZ: f64 = 30.0;

    pub fn run() -> anyhow::Result<()> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("overlay-proto must run on the main thread"))?;
        // Accessory policy: no Dock icon, no self-activation. The proof
        // this driver exists to give is that some OTHER app keeps focus.
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        // The REAL shipping overlay, not a lookalike.
        let overlay = overlay::macos::MacOverlay::new(mtm)?;

        let start = Instant::now();
        let overlay_cell = RefCell::new(overlay);
        println!(
            "overlay-proto: ~{T_END}s scripted dictation at bottom-center. Focus another \
             app and type; keystrokes must keep landing there."
        );

        let tick = RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
            let now = start.elapsed().as_secs_f64();
            if now >= T_END {
                unsafe {
                    let mtm = MainThreadMarker::new_unchecked();
                    NSApplication::sharedApplication(mtm).terminate(None);
                }
                return;
            }
            let state = if now < T_LISTEN {
                OverlayState::ModelLoading
            } else if now < T_TRANSCRIBE {
                OverlayState::Listening
            } else {
                OverlayState::Transcribing
            };
            // Synthetic speech envelope while "speaking"; near-silent
            // during the pause so the orb visibly settles too.
            let level = if (T_LISTEN..T_PAUSE).contains(&now) {
                (((now * 5.0).sin() * 0.5 + 0.5) * 0.7 + 0.15) as f32
            } else {
                0.05
            };
            // The growing hypothesis, exactly as the pipeline publishes it:
            // the whole partial each push. The overlay's own display-side
            // stability policy turns the stable prefix white.
            let hypothesis: Vec<&str> = SCRIPT
                .iter()
                .take_while(|&&(t, _)| t <= now)
                .map(|&(_, w)| w)
                .collect();
            let frame = OverlayFrame {
                state,
                audio_level: level,
                partial_text: hypothesis.join(" "),
                detail: (state == OverlayState::ModelLoading)
                    .then(|| "will transcribe when the model is ready".to_string()),
                // The redesigned macOS overlay pins itself bottom-center;
                // the anchor is carried for the trait's other backends.
                anchor: Anchor::Corner,
            };
            if let Err(e) = overlay_cell.borrow_mut().render(&frame) {
                eprintln!("overlay-proto: render failed: {e}");
            }
        });

        unsafe {
            let timer = NSTimer::timerWithTimeInterval_repeats_block(1.0 / PUSH_HZ, true, &tick);
            NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        }
        app.run();
        Ok(())
    }
}
