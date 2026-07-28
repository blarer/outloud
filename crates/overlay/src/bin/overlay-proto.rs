//! `overlay-proto`: prototype of the orb + rolling-window overlay redesign.
//!
//! Design rationale lives in `docs/overlay-redesign.md`; this binary exists
//! to prove the model on a real screen. Run: `cargo run -p overlay --bin
//! overlay-proto`. It plays a scripted dictation (speech, revision-free
//! commit lag, a long pause, a finalize) over ~26 seconds and exits.
//!
//! Deliberately a NEW binary: another agent is evaluating a framework swap
//! for the shipping overlay, so this prototype must not modify
//! `src/macos.rs` / `src/layout.rs` / `src/theme.rs`. It *reads* the theme
//! (palette, accents) so the redesign stays in the product's visual
//! language, and it reuses the exact non-activation recipe from `macos.rs`
//! because that recipe is the product's core correctness requirement, not
//! styling.

fn main() -> anyhow::Result<()> {
    #[cfg(all(target_os = "macos", feature = "display"))]
    return proto::run();

    #[cfg(not(all(target_os = "macos", feature = "display")))]
    {
        // Same contract as overlay-demo: unsupported platforms get a clean
        // explained exit, because this binary must compile everywhere the
        // workspace builds (including `--no-default-features` headless CI).
        eprintln!(
            "overlay-proto: this prototype needs macOS and the `display` feature. \
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
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{
        class, define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly, Message,
    };
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBezierPath, NSColor,
        NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSPanel, NSScreen,
        NSStatusWindowLevel, NSStringDrawing, NSView, NSWindowCollectionBehavior,
        NSWindowStyleMask,
    };
    use objc2_foundation::{
        NSDictionary, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer,
    };
    use overlay::theme;
    use overlay::OverlayState;

    // ---- Tunables (the numbers docs/overlay-redesign.md defends) ----

    /// Panel size. Wide enough for the 400pt lane plus fade ramp margins,
    /// tall enough for the orb's glow field plus the text lane above it.
    const PANEL_W: f64 = 760.0;
    const PANEL_H: f64 = 230.0;
    /// Left edge of the *newest* word, fixed relative to the panel. Fixed
    /// so the user's glance target never moves; older words extend left.
    const ANCHOR_X: f64 = PANEL_W / 2.0 + 40.0;
    /// Width budget of the text lane, in points (never characters — CJK
    /// units are narrower and a point budget holds more of them).
    const LANE_WIDTH: f64 = 400.0;
    /// Distance past the lane's left edge over which a word ramps 1 -> 0.
    const FADE_RAMP: f64 = 48.0;
    /// Opacity easing time constant. Exponential approach: frame-rate
    /// independent, no start/stop discontinuity, bends smoothly when the
    /// target changes mid-fade. This is the "flowy vs jerky" knob.
    const TAU: f64 = 0.22;
    /// Committed words start decaying this long after commitment: the text
    /// already lives in the target field, so the overlay repeating it
    /// forever is noise. In-flight words never stale-decay.
    const STALE_AFTER: f64 = 4.0;
    /// Duration of the stale decay ramp.
    const STALE_FADE: f64 = 2.0;
    /// Text lane baseline (top-left/flipped coords) and font size.
    const LANE_Y: f64 = 52.0;
    const WORD_FONT: f64 = 17.0;
    const WORD_GAP: f64 = 6.5;
    /// Orb geometry: core radius and how far the glow field reaches.
    const ORB_R: f64 = 21.0;
    const GLOW_R: f64 = 78.0;
    const ORB_CX: f64 = PANEL_W / 2.0;
    const ORB_CY: f64 = PANEL_H - 88.0;

    /// One word in the rolling window.
    struct Word {
        text: String,
        /// Measured width in points, cached at birth (measurement needs an
        /// attribute dictionary; doing it per frame would be waste).
        width: f64,
        /// Whether the commit horizon has settled this word. Committed and
        /// in-flight words are styled differently on purpose: the boundary
        /// between them IS the commit horizon, made visible.
        committed: bool,
        /// Scripted time at which the fake horizon commits this word.
        /// Stored on the word (not derived from an index into SCRIPT)
        /// because overflow fade-out can remove front words while the
        /// script is still feeding, which would shift any index.
        commit_at: f64,
        /// When the word actually committed, for the stale decay.
        committed_at: Option<f64>,
        /// Displayed opacity, eased toward the per-frame target.
        opacity: f64,
    }

    /// Everything `drawRect:` reads. One snapshot struct, same reasoning as
    /// the shipping overlay: a frame is coherent or absent, never partial.
    struct Model {
        state: OverlayState,
        level: f64,
        words: Vec<Word>,
        /// Next SCRIPT entry to spawn. A counter, not `words.len()`:
        /// faded-out words are removed from `words`, so its length says
        /// nothing about how far the script has played.
        next_word: usize,
        now: f64,
        /// Seconds since the previous frame, for the easing integrator.
        dt: f64,
        reduce_motion: bool,
    }

    impl Default for Model {
        fn default() -> Self {
            Model {
                state: OverlayState::ModelLoading,
                level: 0.0,
                words: Vec::new(),
                next_word: 0,
                now: 0.0,
                dt: 0.0,
                reduce_motion: false,
            }
        }
    }

    define_class!(
        /// Same focus-refusal contract as `macos.rs`: the style mask asks
        /// for non-activation, these overrides make it unconditional.
        #[unsafe(super(NSPanel))]
        #[thread_kind = MainThreadOnly]
        #[name = "OrbProtoPanel"]
        struct ProtoPanel;

        impl ProtoPanel {
            #[unsafe(method(canBecomeKeyWindow))]
            fn can_become_key_window(&self) -> bool {
                false
            }

            #[unsafe(method(canBecomeMainWindow))]
            fn can_become_main_window(&self) -> bool {
                false
            }
        }
    );

    define_class!(
        /// Flipped content view: top-left origin matches the layout math.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[name = "OrbProtoView"]
        #[ivars = RefCell<Model>]
        struct ProtoView;

        impl ProtoView {
            #[unsafe(method(isFlipped))]
            fn is_flipped(&self) -> bool {
                true
            }

            #[unsafe(method(drawRect:))]
            fn draw_rect(&self, _dirty: NSRect) {
                self.draw();
            }
        }
    );

    fn ns_color(c: theme::Color, alpha_scale: f64) -> Retained<NSColor> {
        NSColor::colorWithSRGBRed_green_blue_alpha(c.r, c.g, c.b, c.a * alpha_scale)
    }

    /// Measure a word's width with the lane font. Points, not characters:
    /// this is what makes the lane budget CJK-correct for free.
    fn measure(text: &str) -> f64 {
        unsafe {
            let font = NSFont::systemFontOfSize(WORD_FONT);
            let font_obj: Retained<AnyObject> = Retained::into_super(Retained::into_super(font));
            let attrs = NSDictionary::from_retained_objects(&[NSFontAttributeName], &[font_obj]);
            NSString::from_str(text)
                .sizeWithAttributes(Some(&attrs))
                .width
        }
    }

    fn draw_text(text: &str, at: NSPoint, size: f64, color: &NSColor) {
        unsafe {
            let font = NSFont::systemFontOfSize(size);
            let font_obj: Retained<AnyObject> = Retained::into_super(Retained::into_super(font));
            let color_obj: Retained<AnyObject> =
                Retained::into_super(Retained::into_super(color.retain()));
            let attrs = NSDictionary::from_retained_objects(
                &[NSFontAttributeName, NSForegroundColorAttributeName],
                &[font_obj, color_obj],
            );
            NSString::from_str(text).drawAtPoint_withAttributes(at, Some(&attrs));
        }
    }

    /// System Reduce Motion, queried through the runtime rather than the
    /// typed binding: `objc2-app-kit`'s `NSWorkspace` feature is not enabled
    /// in this crate's manifest and the prototype's file budget excludes
    /// `Cargo.toml`. One dynamic call per second is negligible, and reading
    /// it live means flipping the setting mid-session takes effect.
    fn reduce_motion() -> bool {
        unsafe {
            let ws: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
            msg_send![&*ws, accessibilityDisplayShouldReduceMotion]
        }
    }

    impl ProtoView {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(RefCell::new(Model::default()));
            unsafe { msg_send![super(this), init] }
        }

        /// Advance the word opacities one frame. Pure function of
        /// (targets, dt): a dropped frame degrades smoothness, never
        /// correctness. Words that finish fading out are removed, which is
        /// what bounds the model during 30s of continuous speech.
        fn step(model: &mut Model) {
            // Layout right-to-left from the newest word to find each
            // word's left edge, then derive the overflow target from it.
            let mut x = ANCHOR_X;
            let n = model.words.len();
            let mut targets = vec![0.0f64; n];
            for (i, w) in model.words.iter().enumerate().rev() {
                if i + 1 < n {
                    x -= w.width + WORD_GAP;
                }
                let lane_left = ANCHOR_X - LANE_WIDTH;
                // 1. Overflow: fade over one ramp-width past the lane edge.
                let overflow = if x >= lane_left {
                    1.0
                } else {
                    (1.0 - (lane_left - x) / FADE_RAMP).max(0.0)
                };
                // 2. Staleness: committed words drain after a pause;
                //    in-flight words must stay visible until resolved.
                let stale = match w.committed_at {
                    Some(t) => {
                        let age = model.now - t - STALE_AFTER;
                        if age <= 0.0 {
                            1.0
                        } else {
                            (1.0 - age / STALE_FADE).max(0.0)
                        }
                    }
                    None => 1.0,
                };
                targets[i] = overflow.min(stale);
            }
            let ease = 1.0 - (-model.dt / TAU).exp();
            for (w, &t) in model.words.iter_mut().zip(&targets) {
                if model.reduce_motion {
                    // Reduced motion: three discrete tiers, instant steps.
                    // The window still rolls (information preserved); it
                    // just does not continuously animate.
                    w.opacity = if t > 0.66 {
                        1.0
                    } else if t > 0.15 {
                        0.5
                    } else {
                        0.0
                    };
                } else {
                    w.opacity += (t - w.opacity) * ease;
                }
            }
            // Drop dead words only from the front: the rolling window fades
            // oldest-first, and keeping removal ordered keeps layout stable.
            while let Some(w) = model.words.first() {
                if w.opacity < 0.02 && (w.committed || model.words.len() > 1) {
                    model.words.remove(0);
                } else {
                    break;
                }
            }
        }

        fn draw(&self) {
            let model = self.ivars().borrow();
            self.draw_orb(&model);
            self.draw_words(&model);
        }

        /// The orb: a soft radial glow around a bright core. Drawn as
        /// concentric circles with a Gaussian alpha falloff, which at this
        /// size is visually identical to a CAGradientLayer radial gradient;
        /// see docs/overlay-redesign.md §6 for why the prototype does not
        /// add the quartz-core dependency.
        fn draw_orb(&self, model: &Model) {
            let accent = theme::accent(model.state);
            let (glow_gain, core_alpha) = self.orb_dynamics(model);

            if !model.reduce_motion {
                let reach = GLOW_R * (0.72 + 0.28 * glow_gain);
                let rings = 20usize;
                for i in (0..rings).rev() {
                    let f = i as f64 / rings as f64; // 0 center .. 1 edge
                    let r = ORB_R + (reach - ORB_R) * f;
                    // Gaussian falloff reads as light, not as banded disks.
                    let a = (-3.2 * f * f).exp() * 0.16 * (0.55 + 0.45 * glow_gain);
                    ns_color(accent, a).setFill();
                    let rect = NSRect::new(
                        NSPoint::new(ORB_CX - r, ORB_CY - r),
                        NSSize::new(r * 2.0, r * 2.0),
                    );
                    NSBezierPath::bezierPathWithOvalInRect(rect).fill();
                }
            } else if model.state == OverlayState::Listening {
                // Reduced motion: the mic level is shown by ring thickness,
                // a per-frame static quantity, not an oscillation. A stroked
                // circle (not a filled disk) so the level reads as a gauge.
                let thick = 2.0 + 6.0 * model.level;
                let r_mid = ORB_R + 6.0 + thick / 2.0;
                let ring_rect = NSRect::new(
                    NSPoint::new(ORB_CX - r_mid, ORB_CY - r_mid),
                    NSSize::new(r_mid * 2.0, r_mid * 2.0),
                );
                ns_color(accent, 0.8).setStroke();
                let ring = NSBezierPath::bezierPathWithOvalInRect(ring_rect);
                ring.setLineWidth(thick);
                ring.stroke();
            }

            // The core: a filled sphere-read via a bright top highlight
            // over the accent ball. Cylindrical/spherical is the brief.
            ns_color(accent, core_alpha).setFill();
            let core = NSRect::new(
                NSPoint::new(ORB_CX - ORB_R, ORB_CY - ORB_R),
                NSSize::new(ORB_R * 2.0, ORB_R * 2.0),
            );
            NSBezierPath::bezierPathWithOvalInRect(core).fill();
            // Specular highlight, offset up-left: the cheap trick that
            // makes a flat circle read as a sphere.
            let hi_r = ORB_R * 0.62;
            NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.34).setFill();
            let hi = NSRect::new(
                NSPoint::new(ORB_CX - hi_r - 3.0, ORB_CY - hi_r - 5.0),
                NSSize::new(hi_r * 2.0, hi_r * 1.6),
            );
            NSBezierPath::bezierPathWithOvalInRect(hi).fill();
        }

        /// Per-state orb motion, from the table in docs/overlay-redesign.md
        /// §4 — which is the 8-state table of 05-settings-and-states.md,
        /// not a new state machine.
        fn orb_dynamics(&self, model: &Model) -> (f64, f64) {
            if model.reduce_motion {
                // No oscillation of any kind under Reduce Motion.
                let core = match model.state {
                    OverlayState::ModelLoading => 0.5,
                    OverlayState::Transcribing => 1.0,
                    _ => 0.9,
                };
                return (0.0, core);
            }
            match model.state {
                // Glow breathes with the shaped mic level: the orb replaces
                // the bar meter as the "is it hearing me?" surface.
                OverlayState::Listening => (model.level, 0.95),
                // No level input exists; a slow constant shimmer says
                // "machine working, not hung" (UX principle 2).
                OverlayState::Transcribing => {
                    let s = (model.now * std::f64::consts::TAU / 1.2).sin() * 0.5 + 0.5;
                    (0.3 + 0.4 * s, 1.0)
                }
                // The state table's "pulsing" glyph.
                OverlayState::ModelLoading => {
                    let s = (model.now * std::f64::consts::TAU * theme::PULSE_HZ).sin() * 0.5 + 0.5;
                    (s, 0.5 + 0.4 * s)
                }
                // Errors get stillness: motion soothes or celebrates, and
                // an error line should do neither.
                _ => (0.15, 0.9),
            }
        }

        /// The rolling window. Newest word's left edge is pinned at
        /// ANCHOR_X (the glance target never moves); older words extend
        /// left into the fade ramp.
        fn draw_words(&self, model: &Model) {
            let mut x = ANCHOR_X;
            let n = model.words.len();
            for (i, w) in model.words.iter().enumerate().rev() {
                if i + 1 < n {
                    x -= w.width + WORD_GAP;
                }
                if w.opacity <= 0.01 {
                    continue;
                }
                // Committed = settled near-white type. In-flight = accent
                // tint: the tinted zone is the only text allowed to change,
                // making the commit horizon visible (the redesign's point).
                let color = if w.committed {
                    ns_color(theme::Color { a: 0.95, ..theme::palette::PAPER }, w.opacity)
                } else {
                    ns_color(theme::Color { a: 0.62, ..theme::palette::AQUA }, w.opacity)
                };
                draw_text(&w.text, NSPoint::new(x, LANE_Y), WORD_FONT, &color);
            }
        }
    }

    // ---- The scripted dictation ----

    /// (seconds when the word appears in the hypothesis) per word. Commit
    /// lag is applied uniformly below, standing in for the horizon's
    /// stability window. The sentence is the product owner's own example
    /// extended past the lane width, so overflow fading MUST trigger.
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
    /// How long a word stays in-flight before the scripted "horizon"
    /// commits it. Mirrors stability=3 at ~0.4s partial cadence.
    const COMMIT_LAG: f64 = 1.3;
    /// Timeline: model-loading pulse, speech, pause (stale decay drains the
    /// lane), finalize shimmer, exit.
    const T_LISTEN: f64 = 2.0;
    const T_PAUSE: f64 = 11.0;
    const T_TRANSCRIBE: f64 = 19.5;
    const T_END: f64 = 23.5;

    pub fn run() -> anyhow::Result<()> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("prototype must run on the main thread"))?;
        // Accessory policy: no Dock icon, no self-activation. The proof the
        // prototype exists to give is that some OTHER app keeps focus.
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_W, PANEL_H));
        let panel: Retained<ProtoPanel> = unsafe {
            msg_send![
                ProtoPanel::alloc(mtm),
                initWithContentRect: content,
                styleMask: style,
                backing: NSBackingStoreType::Buffered,
                defer: false,
            ]
        };
        panel.setLevel(NSStatusWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        // No window shadow: a shadow draws a rectangle around what should
        // read as a free-floating orb and loose glowing words.
        panel.setHasShadow(false);
        panel.setIgnoresMouseEvents(true);
        panel.setHidesOnDeactivate(false);

        let view = ProtoView::new(mtm);
        panel.setContentView(Some(&view));

        // Bottom-center of the main screen's visibleFrame (clears the
        // Dock). AppKit bottom-left origin, so this math is direct.
        if let Some(screen) = NSScreen::mainScreen(mtm) {
            let vf = screen.visibleFrame();
            let x = vf.origin.x + (vf.size.width - PANEL_W) / 2.0;
            let y = vf.origin.y + 8.0;
            panel.setFrameOrigin(NSPoint::new(x, y));
        }
        // orderFrontRegardless: visible with no activation path at all.
        panel.orderFrontRegardless();

        let start = Instant::now();
        let view_cell = RefCell::new(view.retain());
        let last = RefCell::new(0.0f64);
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
            let view = view_cell.borrow();
            {
                let mut model = view.ivars().borrow_mut();
                model.dt = (now - *last.borrow()).max(0.0);
                model.now = now;
                model.reduce_motion = reduce_motion();
                model.state = if now < T_LISTEN {
                    OverlayState::ModelLoading
                } else if now < T_TRANSCRIBE {
                    OverlayState::Listening
                } else {
                    OverlayState::Transcribing
                };
                // Synthetic speech envelope while "speaking"; near-silent
                // during the pause so the orb visibly settles too.
                model.level = if now < T_PAUSE {
                    ((now * 5.0).sin() * 0.5 + 0.5) * 0.7 + 0.15
                } else {
                    0.05
                };
                // Feed the scripted words in: appear as in-flight, then
                // commit COMMIT_LAG later. Production replaces this loop
                // with `CommitHorizon::update` output.
                while let Some(&(t, text)) = SCRIPT.get(model.next_word) {
                    if t > now {
                        break;
                    }
                    let width = measure(text);
                    model.words.push(Word {
                        text: text.to_string(),
                        width,
                        committed: false,
                        commit_at: t + COMMIT_LAG,
                        committed_at: None,
                        opacity: 0.0, // words are born transparent and bloom in
                    });
                    model.next_word += 1;
                }
                for w in model.words.iter_mut() {
                    if !w.committed && w.commit_at <= now {
                        w.committed = true;
                        w.committed_at = Some(now);
                    }
                }
                ProtoView::step(&mut model);
            }
            *last.borrow_mut() = now;
            view.setNeedsDisplay(true);
        });

        unsafe {
            // 60 Hz while visible; the animations are pure functions of
            // (now, model) so a missed frame costs smoothness only.
            let timer = NSTimer::timerWithTimeInterval_repeats_block(1.0 / 60.0, true, &tick);
            NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        }
        app.run();
        Ok(())
    }
}
