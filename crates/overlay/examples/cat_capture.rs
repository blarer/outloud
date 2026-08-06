//! Drive the REAL `MacOverlay` through representative states and save what
//! its panel's content view actually drew, one PNG per state.
//!
//! Exists because "the cat renders correctly in every state" needs eyes on
//! pixels, and `screencapture` needs a Screen Recording grant the daemon's
//! sandbox may not have. `cacheDisplayInRect:toBitmapImageRep:` runs the
//! same `drawRect:` the compositor runs — aura, gradients, shadows and all
//! — so these PNGs are the panel's real output, not a parallel renderer's.
//!
//! Run: `cargo run -p overlay --example cat_capture` → `/tmp/overlay_*.png`.

fn main() -> anyhow::Result<()> {
    #[cfg(all(target_os = "macos", feature = "display"))]
    return capture::run();

    #[cfg(not(all(target_os = "macos", feature = "display")))]
    {
        eprintln!("cat_capture: needs macOS and the `display` feature.");
        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "display"))]
mod capture {
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBitmapImageFileType, NSBitmapImageRep,
        NSView, NSWindow,
    };
    use objc2_foundation::{NSDictionary, NSString};
    use overlay::{Anchor, Overlay, OverlayFrame, OverlayState};

    /// The scripted moments to capture: state, level, hypothesis, and how
    /// long to let the animator settle first (long enough for ears and
    /// pupils to reach their per-state posture through their envelopes).
    const SHOTS: &[(&str, OverlayState, f32, &str)] = &[
        (
            "listening_quiet",
            OverlayState::Listening,
            0.05,
            "the dog is",
        ),
        (
            "listening_loud",
            OverlayState::Listening,
            0.95,
            "the dog is brown and has",
        ),
        (
            "transcribing",
            OverlayState::Transcribing,
            0.0,
            "the dog is brown and has a lot of fun",
        ),
        ("error", OverlayState::Error, 0.0, ""),
        ("no_permission", OverlayState::NoPermission, 0.0, ""),
        ("model_loading", OverlayState::ModelLoading, 0.0, ""),
    ];

    pub fn run() -> anyhow::Result<()> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("cat_capture must run on the main thread"))?;
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let mut overlay = overlay::macos::MacOverlay::new(mtm)?;

        for &(name, state, level, text) in SHOTS {
            let detail = match state {
                OverlayState::Error => Some("transcription failed → try again".to_string()),
                OverlayState::NoPermission => {
                    Some("microphone access needed → System Settings".to_string())
                }
                OverlayState::ModelLoading => {
                    Some("will transcribe when the model is ready".to_string())
                }
                _ => None,
            };
            // Push the same frame repeatedly for ~1.5s of wall time so the
            // ear/pupil envelopes settle and the entry animation finishes;
            // spinning the run loop lets the display link tick.
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
            while std::time::Instant::now() < deadline {
                overlay.render(&OverlayFrame {
                    state,
                    audio_level: level,
                    partial_text: text.to_string(),
                    detail: detail.clone(),
                    anchor: Anchor::Corner,
                })?;
                unsafe {
                    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};
                    NSRunLoop::currentRunLoop().runMode_beforeDate(
                        NSDefaultRunLoopMode,
                        &NSDate::dateWithTimeIntervalSinceNow(0.03),
                    );
                }
            }
            save_view_png(&overlay, name)?;
            println!("/tmp/overlay_{name}.png");
        }
        // Leave nothing on screen.
        overlay.hide()?;
        Ok(())
    }

    /// Snapshot the panel's content view through the same `drawRect:` the
    /// compositor uses.
    fn save_view_png(overlay: &overlay::macos::MacOverlay, name: &str) -> anyhow::Result<()> {
        let panel: &NSWindow = overlay.panel_for_capture();
        let view: Retained<NSView> = panel
            .contentView()
            .ok_or_else(|| anyhow::anyhow!("panel has no content view"))?;
        let bounds = view.bounds();
        unsafe {
            let rep: Option<Retained<NSBitmapImageRep>> =
                view.bitmapImageRepForCachingDisplayInRect(bounds);
            let rep = rep.ok_or_else(|| anyhow::anyhow!("no bitmap rep"))?;
            view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
            let props: Retained<NSDictionary<_, objc2::runtime::AnyObject>> = NSDictionary::new();
            let data = rep
                .representationUsingType_properties(NSBitmapImageFileType::PNG, &props)
                .ok_or_else(|| anyhow::anyhow!("png encode failed"))?;
            let path = format!("/tmp/overlay_{name}.png");
            data.writeToFile_atomically(&NSString::from_str(&path), true);
        }
        Ok(())
    }
}
