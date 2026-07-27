//! Criterion benchmarks for the macOS accessibility hot path.
//!
//! Every number that matters here is the cost of one synchronous IPC round
//! trip into another process, so the benchmarks are deliberately structured
//! around *individual calls*: resolve the focused application, resolve the
//! focused element, read each attribute separately, probe settability, batch
//! the reads, and write. That decomposition is what lets us say which call
//! dominates instead of guessing.
//!
//! Requirements to produce real numbers (all checked at startup):
//!   - macOS, with this process trusted for Accessibility (run from a trusted
//!     terminal or via scripts/bench-latency.sh).
//!   - A text field focused in the frontmost application. The script focuses
//!     TextEdit with a document open, which is the native-AppKit baseline.
//!
//! If those preconditions fail the harness exits with a message rather than
//! benchmarking error paths, because timing a failed IPC (which returns
//! immediately or after the messaging timeout) would produce numbers that look
//! plausible and mean nothing.

// The whole harness is macOS-only: on other platforms there is no
// accessibility backend to measure, so the bench compiles to an empty main
// and CI's cross-platform builds stay green.
#[cfg(target_os = "macos")]
mod mac {

    use std::time::Duration;

    use criterion::{criterion_group, Criterion};

    use accessibility_sys::{
        kAXErrorSuccess, kAXFocusedApplicationAttribute, kAXFocusedUIElementAttribute,
        kAXNumberOfCharactersAttribute, kAXRoleAttribute, kAXSelectedTextAttribute,
        kAXSelectedTextRangeAttribute, kAXValueAttribute, AXIsProcessTrusted,
        AXUIElementCopyAttributeValue, AXUIElementCopyMultipleAttributeValues,
        AXUIElementCreateApplication, AXUIElementCreateSystemWide, AXUIElementIsAttributeSettable,
        AXUIElementRef, AXUIElementSetAttributeValue, AXUIElementSetMessagingTimeout,
    };
    use core_foundation::array::CFArray;
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::array::CFArrayRef;
    use core_foundation_sys::base::{CFRelease, CFTypeRef};
    use libc::pid_t;

    /// Owned AXUIElementRef, mirroring the ownership rule in ax_edit::macos:
    /// everything from a Copy/Create call is released exactly once.
    struct El(AXUIElementRef);
    impl Drop for El {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0 as CFTypeRef) };
            }
        }
    }

    /// Owned CFTypeRef.
    struct Val(CFTypeRef);
    impl Drop for Val {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn with_timeout(el: El) -> El {
        // Same 0.5s cap the production code uses, so a hung target cannot make a
        // benchmark run take minutes. None of the benchmarked calls hit it in a
        // healthy target.
        unsafe { AXUIElementSetMessagingTimeout(el.0, 0.5) };
        el
    }

    /// One attribute read: the unit of IPC this whole file exists to measure.
    fn copy_attr(el: AXUIElementRef, name: &str) -> Option<Val> {
        let cf_name = CFString::new(name);
        let mut raw: CFTypeRef = std::ptr::null();
        let code =
            unsafe { AXUIElementCopyAttributeValue(el, cf_name.as_concrete_TypeRef(), &mut raw) };
        if code == kAXErrorSuccess && !raw.is_null() {
            Some(Val(raw))
        } else {
            None
        }
    }

    /// Batched read of several attributes in ONE IPC round trip. This is the
    /// hypothesis under test: if per-call overhead dominates, five reads collapse
    /// to roughly the cost of one.
    fn copy_attrs_batched(el: AXUIElementRef, names: &[&str]) -> Option<Val> {
        let cf_names: Vec<CFString> = names.iter().map(|n| CFString::new(n)).collect();
        let array = CFArray::from_CFTypes(&cf_names);
        let mut out: CFArrayRef = std::ptr::null();
        // options=0: missing attributes come back as AXValue error placeholders in
        // the array instead of failing the whole call, matching how the per-call
        // path treats an absent attribute as a normal outcome.
        let code = unsafe {
            AXUIElementCopyMultipleAttributeValues(el, array.as_concrete_TypeRef(), 0, &mut out)
        };
        if code == kAXErrorSuccess && !out.is_null() {
            Some(Val(out as CFTypeRef))
        } else {
            None
        }
    }

    fn is_settable(el: AXUIElementRef, name: &str) -> bool {
        let cf_name = CFString::new(name);
        let mut settable = false;
        let code = unsafe {
            AXUIElementIsAttributeSettable(el, cf_name.as_concrete_TypeRef(), &mut settable)
        };
        code == kAXErrorSuccess && settable
    }

    /// Resolve the frontmost application element, exactly the two-route order
    /// production takes: system-wide AXFocusedApplication first, then the
    /// CGWindowList pid fallback. On this machine the system-wide route fails
    /// (the M0 finding about the stricter system-wide path), so the fallback is
    /// the one actually exercised, and both are benchmarked separately below.
    fn focused_app() -> Option<El> {
        if let Some(app) = focused_app_via_systemwide() {
            return Some(app);
        }
        focused_app_via_window_list()
    }

    /// Route 1: ask the system-wide element for AXFocusedApplication (one IPC).
    fn focused_app_via_systemwide() -> Option<El> {
        let system = with_timeout(El(unsafe { AXUIElementCreateSystemWide() }));
        if system.0.is_null() {
            return None;
        }
        let v = copy_attr(system.0, kAXFocusedApplicationAttribute)?;
        let raw = v.0 as AXUIElementRef;
        std::mem::forget(v);
        Some(with_timeout(El(raw)))
    }

    /// Route 2: frontmost pid from the CoreGraphics window list, then a local
    /// AXUIElementCreateApplication. The window list call goes to the window
    /// server, which is its own IPC with its own cost.
    fn focused_app_via_window_list() -> Option<El> {
        let pid = frontmost_pid_from_window_list()?;
        let app = El(unsafe { AXUIElementCreateApplication(pid) });
        if app.0.is_null() {
            return None;
        }
        Some(with_timeout(app))
    }

    fn frontmost_pid_from_window_list() -> Option<pid_t> {
        use core_foundation::base::CFType;
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::number::CFNumber;

        extern "C" {
            fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> CFArrayRef;
        }
        const ON_SCREEN_ONLY: u32 = 1 << 0;
        const EXCLUDE_DESKTOP: u32 = 1 << 4;

        let info = unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0) };
        if info.is_null() {
            return None;
        }
        let windows: CFArray<CFDictionary> = unsafe { CFArray::wrap_under_create_rule(info) };
        let layer_key = CFString::new("kCGWindowLayer");
        let pid_key = CFString::new("kCGWindowOwnerPID");
        for w in windows.iter() {
            let layer = w
                .find(layer_key.as_CFTypeRef() as *const _)
                .and_then(|v| unsafe { CFType::wrap_under_get_rule(*v) }.downcast::<CFNumber>())
                .and_then(|n| n.to_i64());
            if layer != Some(0) {
                continue;
            }
            if let Some(pid) = w
                .find(pid_key.as_CFTypeRef() as *const _)
                .and_then(|v| unsafe { CFType::wrap_under_get_rule(*v) }.downcast::<CFNumber>())
                .and_then(|n| n.to_i64())
            {
                return Some(pid as pid_t);
            }
        }
        None
    }

    /// Resolve the focused element within an application (one IPC).
    fn focused_in(app: &El) -> Option<El> {
        let v = copy_attr(app.0, kAXFocusedUIElementAttribute)?;
        let raw = v.0 as AXUIElementRef;
        std::mem::forget(v);
        Some(with_timeout(El(raw)))
    }

    /// The five attributes snapshot_focused reads off the focused element.
    const SNAPSHOT_ATTRS: [&str; 5] = [
        kAXRoleAttribute,
        kAXValueAttribute,
        kAXSelectedTextAttribute,
        kAXSelectedTextRangeAttribute,
        kAXNumberOfCharactersAttribute,
    ];

    fn bench_resolution(c: &mut Criterion) {
        let mut g = c.benchmark_group("resolve");
        g.sample_size(30).measurement_time(Duration::from_secs(4));

        // AXUIElementCreateSystemWide is documented as local (no IPC); measured to
        // confirm rather than assumed.
        g.bench_function("create_system_wide", |b| {
            b.iter(|| {
                let el = El(unsafe { AXUIElementCreateSystemWide() });
                std::hint::black_box(&el);
            })
        });

        g.bench_function("focused_application", |b| {
            b.iter(|| std::hint::black_box(focused_app().expect("focused app")))
        });

        // The two routes measured apart, because production tries them in
        // order and the failing first attempt is pure overhead when the
        // fallback ends up doing the work.
        g.bench_function("route_systemwide_focused_app", |b| {
            b.iter(|| std::hint::black_box(focused_app_via_systemwide()))
        });
        g.bench_function("route_window_list_pid", |b| {
            b.iter(|| std::hint::black_box(focused_app_via_window_list().expect("window list app")))
        });

        // The candidate replacement for repeated resolution: keep the app element
        // and only re-ask it for its focused element.
        let app = focused_app().expect("focused app");
        g.bench_function("focused_element_given_app", |b| {
            b.iter(|| std::hint::black_box(focused_in(&app).expect("focused element")))
        });

        g.bench_function("focused_element_full", |b| {
            b.iter(|| {
                let app = focused_app().expect("focused app");
                std::hint::black_box(focused_in(&app).expect("focused element"))
            })
        });
        g.finish();
    }

    fn bench_attribute_reads(c: &mut Criterion) {
        let app = focused_app().expect("focused app");
        let el = focused_in(&app).expect("focused element");

        let mut g = c.benchmark_group("attr_read");
        g.sample_size(30).measurement_time(Duration::from_secs(4));

        // Each attribute measured alone, because "the read costs 30ms" hides
        // whether one attribute is slow or six round trips add up.
        for name in SNAPSHOT_ATTRS {
            g.bench_function(name, |b| {
                b.iter(|| std::hint::black_box(copy_attr(el.0, name)))
            });
        }

        // AXTitle is read off the *application* element for TextSnapshot::app.
        g.bench_function("AXTitle_on_app", |b| {
            b.iter(|| std::hint::black_box(copy_attr(app.0, accessibility_sys::kAXTitleAttribute)))
        });

        // The batching hypothesis: five attributes in one round trip.
        g.bench_function("batched_5_attrs", |b| {
            b.iter(|| {
                std::hint::black_box(copy_attrs_batched(el.0, &SNAPSHOT_ATTRS).expect("batch"))
            })
        });
        g.finish();
    }

    fn bench_settable_probes(c: &mut Criterion) {
        let app = focused_app().expect("focused app");
        let el = focused_in(&app).expect("focused element");

        let mut g = c.benchmark_group("settable");
        g.sample_size(30).measurement_time(Duration::from_secs(4));

        // Two more IPC round trips per snapshot. If each costs about the same as
        // an attribute read, dropping them from the hot path (try the write,
        // handle failure) saves the same again.
        g.bench_function("AXValue", |b| {
            b.iter(|| std::hint::black_box(is_settable(el.0, kAXValueAttribute)))
        });
        g.bench_function("AXSelectedText", |b| {
            b.iter(|| std::hint::black_box(is_settable(el.0, kAXSelectedTextAttribute)))
        });
        g.finish();
    }

    fn bench_write(c: &mut Criterion) {
        // Focus can drift during a long run; skip rather than panic so the
        // remaining groups still report.
        let Some(app) = focused_app() else {
            eprintln!("write: no focused app, group skipped");
            return;
        };
        let Some(el) = focused_in(&app) else {
            eprintln!("write: no focused element, group skipped");
            return;
        };

        // Read the current value and write the same text back, so the benchmark
        // exercises the real write path without corrupting the operator's
        // document. Requires the focused field to be writable (TextEdit is).
        let Some(current) = copy_attr(el.0, kAXValueAttribute) else {
            eprintln!("write: focused field has no value, group skipped");
            return;
        };
        let text = unsafe {
            core_foundation::base::CFType::wrap_under_get_rule(current.0)
                .downcast::<CFString>()
                .expect("value is a string")
                .to_string()
        };

        let mut g = c.benchmark_group("write");
        // Writes trigger real layout/undo work in the target, so they are slower
        // and noisier: fewer samples, longer window.
        g.sample_size(20).measurement_time(Duration::from_secs(6));
        g.bench_function("set_AXValue_same_text", |b| {
            let cf_name = CFString::new(kAXValueAttribute);
            b.iter(|| {
                let cf_text = CFString::new(&text);
                let code = unsafe {
                    AXUIElementSetAttributeValue(
                        el.0,
                        cf_name.as_concrete_TypeRef(),
                        cf_text.as_CFTypeRef(),
                    )
                };
                assert_eq!(
                    code, kAXErrorSuccess,
                    "write must succeed to be a valid sample"
                );
            })
        });
        g.finish();
    }

    /// Full snapshot through the public API, before/after any implementation
    /// change. This is the number the product actually pays per dictation.
    fn bench_snapshot_full(c: &mut Criterion) {
        // Probe once first: snapshot_focused re-resolves focus on every call,
        // so if focus drifted away from the text field mid-run (a notification,
        // a Space switch) this group must skip rather than panic and take the
        // remaining groups down with it.
        if let Err(e) = ax_edit::snapshot_focused() {
            eprintln!("snapshot: focused field unavailable ({e}), group skipped");
            return;
        }
        let mut g = c.benchmark_group("snapshot");
        g.sample_size(30).measurement_time(Duration::from_secs(5));
        g.bench_function("snapshot_focused", |b| {
            b.iter(|| std::hint::black_box(ax_edit::snapshot_focused().expect("snapshot")))
        });
        g.finish();
    }

    /// Same attribute reads against other application families, reached by pid so
    /// no window choreography is needed: an app element answers for its own
    /// focused element even when it is not frontmost.
    fn bench_cross_app(c: &mut Criterion) {
        let mut g = c.benchmark_group("cross_app");
        g.sample_size(20).measurement_time(Duration::from_secs(4));

        for (label, pattern) in [
            ("safari", "Safari.app/Contents/MacOS/Safari"),
            ("chrome", "Google Chrome.app/Contents/MacOS/Google Chrome"),
            ("discord_electron", "Discord.app/Contents/MacOS/Discord"),
        ] {
            let Some(pid) = pid_matching(pattern) else {
                eprintln!("cross_app/{label}: not running, skipped");
                continue;
            };
            let app = with_timeout(El(unsafe { AXUIElementCreateApplication(pid) }));
            if app.0.is_null() {
                continue;
            }
            // Chromium apps expose no tree until asked; same opt-in production uses.
            let key = CFString::new("AXManualAccessibility");
            unsafe {
                AXUIElementSetAttributeValue(
                    app.0,
                    key.as_concrete_TypeRef(),
                    core_foundation::boolean::CFBoolean::true_value().as_CFTypeRef(),
                );
            }
            // The application element itself always has AXRole; the focused
            // element may not exist for a background app, so benchmark the app
            // element read, which is the same IPC shape.
            g.bench_function(format!("{label}/AXRole_on_app"), |b| {
                b.iter(|| std::hint::black_box(copy_attr(app.0, kAXRoleAttribute)))
            });
            g.bench_function(format!("{label}/batched_5_on_app"), |b| {
                b.iter(|| std::hint::black_box(copy_attrs_batched(app.0, &SNAPSHOT_ATTRS)))
            });
        }
        g.finish();
    }

    fn pid_matching(pattern: &str) -> Option<pid_t> {
        let out = std::process::Command::new("pgrep")
            .args(["-f", pattern])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.trim().parse().ok())
    }

    fn guard_preconditions() {
        if !unsafe { AXIsProcessTrusted() } {
            eprintln!(
                "SKIP: process not trusted for Accessibility; run via scripts/bench-latency.sh"
            );
            std::process::exit(0);
        }

        // Cold-path numbers, taken before criterion warms anything up.
        // Criterion's iteration model can only report steady-state, but the
        // product's first snapshot after launch pays the cold cost: the AX
        // connection to the target is established lazily on first message,
        // and the M0 measurement (25-33ms read) was exactly this cold path.
        // One-shot timings are noisy by nature, so they are labelled as such.
        let t0 = std::time::Instant::now();
        let Some(app) = focused_app() else {
            eprintln!("SKIP: no focused application");
            std::process::exit(0);
        };
        let cold_app = t0.elapsed();
        let t1 = std::time::Instant::now();
        if focused_in(&app).is_none() {
            eprintln!(
                "SKIP: no focused element; focus a text field (scripts/bench-latency.sh does this)"
            );
            std::process::exit(0);
        }
        let cold_focused = t1.elapsed();
        let t2 = std::time::Instant::now();
        let snap = ax_edit::snapshot_focused();
        let cold_snapshot = t2.elapsed();
        eprintln!(
            "COLD one-shot: focused_app={cold_app:?} focused_element={cold_focused:?} \
             snapshot={cold_snapshot:?} (ok={})",
            snap.is_ok()
        );
    }

    fn all(c: &mut Criterion) {
        guard_preconditions();
        bench_resolution(c);
        bench_attribute_reads(c);
        bench_settable_probes(c);
        bench_snapshot_full(c);
        bench_write(c);
        bench_cross_app(c);
    }

    criterion_group!(benches, all);
}

#[cfg(target_os = "macos")]
criterion::criterion_main!(mac::benches);

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("ax_latency bench is macOS-only");
}
