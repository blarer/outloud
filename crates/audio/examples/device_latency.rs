//! Where the milliseconds go between opening an input device and that device
//! actually hearing the room, broken down by phase, for any device present.
//!
//! `first_sample_latency.rs` answers "how long until the ring has samples" for
//! the *default* device. That was enough to clear the built-in microphone and
//! not enough for anything else, for two reasons:
//!
//! 1. **It cannot reach a non-default device.** `audio::capture::start_capture`
//!    always opens `default_input_device()`, by design. Measuring a Bluetooth
//!    headset through it means making that headset the system default first,
//!    by hand, in System Settings, between runs.
//! 2. **"A buffer arrived" is not "the device is hearing you".** CoreAudio
//!    hands the callback a buffer on schedule whether or not the capture chain
//!    behind it has converged. Worse, measured here, the *first* buffer after a
//!    cold open is frequently stale: it contains audio from before the open.
//!    Proven by muting the reference tone for 2.5 seconds and then opening, at
//!    which point buffer 1 still arrived full of tone in 14 of 15 runs. So the
//!    first buffer is not merely an optimistic measure of when capture began,
//!    it can be evidence of a moment that has already passed.
//!
//! So the decisive number here comes from an acoustic loopback rather than from
//! staring at the input and guessing. A reference tone plays continuously from
//! the default output device *before* the input is opened, so the sound already
//! exists in the room at T0. Then the input opens and every buffer is scored
//! for energy at that exact frequency (Goertzel). The first *sustained* run of
//! buffers containing the tone is the first moment this device was really
//! listening.
//!
//! Sustained, not first, precisely because of the stale prefill above: a single
//! strong buffer followed by silence is the artifact, not the signal.
//!
//! ```text
//!   enumerate   host lookup + description
//!   config      default_input_config()      <- CoreAudio may block here
//!   build       build_input_stream()
//!   play        stream.play()
//!   1st buffer  first data callback         <- what first_sample_latency times
//!   stale?      was that first buffer pre-open audio
//!   live tone   first sustained run of buffers hearing the tone  <- the answer
//! ```
//!
//! `live tone - 1st buffer` is audio a user spoke, that the daemon believes it
//! captured, and that is not really there. No downstream buffer recovers it.
//!
//! # Calibration, and why it is not optional
//!
//! The tone's level at the microphone depends on speaker volume, room, and how
//! far away the device is, so no fixed threshold can be right. An early version
//! of this example used one, and on a quiet setting it reported the built-in
//! microphone taking 250ms to hear anything. That number was false: the
//! threshold was sitting inside the tone's own ripple, so it flapped. With the
//! detector calibrated against the same device's steady-state level, the true
//! answer was 55ms.
//!
//! So every device is calibrated first: hold the stream open, take the median
//! tone level once it is steady, and set the detection threshold to a quarter
//! of that. A device whose steady level is not clearly above the room is
//! reported as uncalibratable rather than measured, because a number produced
//! by a flapping detector is worse than no number.
//!
//! # Running it
//!
//! Every input device on the machine:
//!
//! ```bash
//! cargo run --release -p audio --example device_latency
//! ```
//!
//! One device (case-insensitive substring, so `airpods` is enough):
//!
//! ```bash
//! cargo run --release -p audio --example device_latency -- airpods
//! ```
//!
//! Without the loopback, when you cannot make noise (`--no-tone`). This still
//! reports every phase and the first-buffer time; it just cannot tell you when
//! the device really started hearing, which is the number that decides
//! anything:
//!
//! ```bash
//! cargo run --release -p audio --example device_latency -- --no-tone
//! ```
//!
//! **Turn the volume up to something clearly audible before running.** The tone
//! has to survive the trip through the room. A muted machine fails calibration
//! and says so, rather than reporting a fast device.
//!
//! **Measuring a Bluetooth input:** play the tone from the *built-in speakers*,
//! not through the headset. macOS switches a headset into its hands-free
//! profile when either direction opens, so a tone playing through the headset
//! has already paid the profile switch this is trying to measure, and the run
//! comes back flatteringly fast. Set Sound > Output to the built-in speakers
//! and Sound > Input to the headset.
//!
//! Runs are cold on purpose: the daemon opens the microphone on every key-down
//! (`crates/outloud/src/mic.rs`), so a warm reopen is not the case that matters.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Open/close cycles per device. Enough to see spread without making whoever
/// runs this sit through a long session per headset.
const RUNS: usize = 10;

/// Abandon a run after this. Bluetooth hands-free negotiation is slow, but it
/// is not this slow, and one hung open must not hang the whole sweep.
const TIMEOUT: Duration = Duration::from_secs(6);

/// Settle time between runs, so each open is cold the way the daemon's is.
/// Bluetooth in particular holds its profile briefly after the last client
/// closes, and measuring inside that grace period would flatter the result.
const SETTLE: Duration = Duration::from_millis(600);

/// Reference tone frequency. 1kHz sits in the middle of what every microphone
/// and every headset codec passes cleanly, including the narrowband hands-free
/// profile a Bluetooth headset switches into, whose ceiling is around 4kHz. A
/// higher tone would be more selective and would risk being filtered away by
/// the very profile switch this exists to measure.
const TONE_HZ: f32 = 1_000.0;

/// Tone amplitude. Loud enough to clear a room's noise floor by an order of
/// magnitude, quiet enough not to be unpleasant for the person sitting there.
const TONE_AMPLITUDE: f32 = 0.3;

/// How long the tone plays before the first input open, so the sound is already
/// established in the room at T0 rather than ramping up alongside the thing
/// being measured.
const TONE_LEAD_IN: Duration = Duration::from_millis(800);

/// How long calibration listens in each of its two phases (tone muted, then
/// tone playing). Long enough to cover the input filter's settling transient
/// after the open, which on the built-in microphone runs for roughly the first
/// 150ms, and still leave a solid steady-state majority to take a median from.
const CALIBRATION_WINDOW: Duration = Duration::from_millis(1_200);

/// Ratio by which the tone must exceed the room's own level at the same
/// frequency before the measurement is trusted. Below this, the device cannot
/// really hear the speakers and any timing taken from it would be noise.
const MIN_TONE_OVER_ROOM: f32 = 4.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let use_tone = !args.iter().any(|a| a == "--no-tone");
    let filter = args.iter().find(|a| !a.starts_with("--")).cloned();

    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else {
        println!("could not enumerate input devices; is audio available here?");
        return;
    };
    let default_name = host
        .default_input_device()
        .and_then(|d| d.description().ok().map(|x| x.name().to_string()));

    let targets: Vec<cpal::Device> = devices
        .filter(|d| match (&filter, d.description().ok()) {
            (Some(f), Some(desc)) => desc.name().to_lowercase().contains(&f.to_lowercase()),
            (Some(_), None) => false,
            (None, _) => true,
        })
        .collect();

    if targets.is_empty() {
        match filter {
            Some(f) => println!("no input device matches {f:?}. Connect it, then re-run."),
            None => println!("no input devices on this machine."),
        }
        return;
    }

    println!("pre-roll window: {}ms", pre_roll_ms());

    // Held for the whole sweep: the tone must already exist in the room before
    // any input stream opens, or the measurement would be racing the reference
    // against itself.
    let tone = if use_tone {
        match start_tone(&host) {
            Ok(t) => {
                println!("reference tone: {TONE_HZ:.0}Hz from {}", t.device_name);
                std::thread::sleep(TONE_LEAD_IN);
                Some(t)
            }
            Err(e) => {
                println!("could not start the reference tone ({e}).");
                println!("Falling back to first-buffer timing only, which cannot tell you");
                println!("when the device really started hearing. See the module docs.");
                None
            }
        }
    } else {
        println!("reference tone: disabled (--no-tone)");
        None
    };
    println!();

    for device in targets {
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "<unnamed>".into());
        let is_default = Some(&name) == default_name.as_ref();
        println!(
            "=== {name}{} ===",
            if is_default { "  (system default)" } else { "" }
        );
        // Printed so a reader can tell a Bluetooth row from a USB one without
        // trusting the device name. cpal's CoreAudio backend only fills this in
        // for aggregate devices today (cpal-0.18.1
        // src/host/coreaudio/macos/device.rs:424), so on macOS it is usually
        // Unknown and the name is the real signal. Shown anyway: it costs
        // nothing and becomes correct for free if cpal grows the property read.
        if let Ok(desc) = device.description() {
            println!(
                "    interface: {:?}   type: {:?}",
                desc.interface_type(),
                desc.device_type()
            );
        }
        measure_device(&device, tone.as_ref());
        println!();
    }

    println!("Reading this:");
    println!("  '1st buffer' is when the callback first ran: what the daemon's");
    println!("  StartupWatch measures today (crates/outloud/src/devlatency.rs:84).");
    if tone.is_some() {
        println!("  'live tone' is when a sustained run of buffers first contained the");
        println!("  reference tone that was already playing before the stream opened.");
        println!("  Speech before that instant was not captured by anyone, and no");
        println!("  pre-roll recovers it. 'stale?' flags runs where buffer 1 held audio");
        println!("  from before the open, which is why one buffer is never enough.");
    } else {
        println!("  'live tone' was not measured. Without it, a device that delivers");
        println!("  punctual buffers of nothing looks identical to a fast one.");
    }
}

/// One device's worth of runs, printed per run and then summarized.
fn measure_device(device: &cpal::Device, tone: Option<&Tone>) {
    // Calibrate before timing anything: see the module docs for the false
    // 250ms result a fixed threshold produced.
    let threshold = if let Some(tone) = tone {
        match calibrate(device, tone) {
            Ok(t) => {
                println!(
                    "  calibrated: tone {:.6} vs room {:.6} -> detect above {:.6}",
                    t.tone_level, t.room_level, t.threshold
                );
                Some(t.threshold)
            }
            Err(e) => {
                println!("  CALIBRATION FAILED: {e}");
                println!("  Reporting phases and first-buffer only. No verdict: a timing taken");
                println!("  with an uncalibrated detector is worse than no timing.");
                None
            }
        }
    } else {
        None
    };

    let mut runs = Vec::with_capacity(RUNS);
    println!(
        "  {:>4}  {:>9} {:>9} {:>9} {:>9} {:>11} {:>7} {:>10}",
        "run", "enumerate", "config", "build", "play", "1st buffer", "stale?", "live tone"
    );
    for i in 1..=RUNS {
        match time_open(device, threshold) {
            Ok(p) => {
                println!(
                    "  {i:>4}  {:>8.1}ms {:>8.1}ms {:>8.1}ms {:>8.1}ms {:>10.1}ms {:>7} {}",
                    ms(p.enumerate),
                    ms(p.config),
                    ms(p.build),
                    ms(p.play),
                    ms(p.first_buffer),
                    match (threshold.is_some(), p.stale_prefill) {
                        (false, _) => "-",
                        (true, true) => "YES",
                        (true, false) => "no",
                    },
                    match (threshold.is_some(), p.live_tone) {
                        (false, _) => format!("{:>10}", "-"),
                        (true, Some(t)) => format!("{:>8.1}ms", ms(t)),
                        (true, None) => format!("{:>10}", "NOT HEARD"),
                    }
                );
                runs.push(p);
            }
            Err(e) => println!("  {i:>4}  FAILED: {e}"),
        }
        std::thread::sleep(SETTLE);
    }

    if runs.is_empty() {
        println!("\n  no successful runs on this device.");
        return;
    }

    let buffers: Vec<Duration> = runs.iter().map(|p| p.first_buffer).collect();
    let tones: Vec<Duration> = runs.iter().filter_map(|p| p.live_tone).collect();
    let stale_count = runs.iter().filter(|p| p.stale_prefill).count();

    println!("\n  n={}", runs.len());
    summarize("1st buffer", &buffers);

    if threshold.is_none() {
        println!(
            "\n  No verdict: without a calibrated reference tone this cannot distinguish\n  \
             a device that is hearing you from one handing out punctual empty buffers."
        );
        return;
    }

    if stale_count > 0 {
        println!(
            "  stale first buffer in {stale_count}/{} runs: buffer 1 contained audio\n  \
             from before the stream was opened, then the device went quiet again.",
            runs.len()
        );
    }

    if tones.is_empty() {
        println!(
            "  live tone: NEVER, in any run, despite calibrating. The device hears the\n  \
             tone when held open but not within {TIMEOUT:?} of a cold open. That is a\n  \
             real and severe result: this device captures nothing for at least that\n  \
             long after it is opened."
        );
        return;
    }
    summarize("live tone", &tones);
    if tones.len() < runs.len() {
        println!(
            "  ({} of {} runs never heard the tone within {TIMEOUT:?})",
            runs.len() - tones.len(),
            runs.len()
        );
    }

    let tone_p50 = percentile(&tones, 0.5);
    let buffer_p50 = percentile(&buffers, 0.5);
    let pre_roll = pre_roll_ms() as f64;
    println!();
    if ms(tone_p50) < pre_roll {
        println!("  VERDICT: fine. This device hears the room inside the {pre_roll:.0}ms pre-roll");
        println!("  window, so a word begun the instant the key goes down survives whole.");
    } else if ms(tone_p50) < 400.0 {
        println!(
            "  VERDICT: marginal. The device hears nothing until {:.0}ms, past the",
            ms(tone_p50)
        );
        println!("  {pre_roll:.0}ms pre-roll, so a user who speaks immediately loses the first");
        println!("  syllable. A half-captured word is misrecognised, not dropped");
        println!("  (docs/input-latency.md).");
    } else {
        println!("  VERDICT: bad. Over 400ms before this device hears anything. Every");
        println!("  utterance loses its opening word unless the stream is already warm");
        println!("  when the key goes down.");
    }

    let hidden = ms(tone_p50) - ms(buffer_p50);
    if hidden > 30.0 {
        println!();
        println!("  NOTE: {hidden:.0}ms of this is invisible to the daemon's watchdog. Buffers");
        println!(
            "  arrive at {:.0}ms and StartupWatch stops its clock there",
            ms(buffer_p50)
        );
        println!("  (crates/outloud/src/devlatency.rs:84), but the device is not hearing");
        println!(
            "  anything until {:.0}ms. The watchdog under-reports by that gap.",
            ms(tone_p50)
        );
    }

    // The number that decides whether keeping a stream warm would help at all.
    // Without it, "hold the device open" is a guess; with it, the benefit is a
    // measurement.
    if let Some(t) = threshold {
        match warm_open(device, t) {
            Ok(warm) => {
                println!();
                println!(
                    "  WARM COMPARISON: with another stream already open on this device, a\n  \
                     second cold open hears the tone in {:.0}ms, against {:.0}ms cold.",
                    ms(warm),
                    ms(tone_p50)
                );
                println!(
                    "  So {:.0}ms of the cold number is the device starting up, not this\n  \
                     process opening a stream. That is what a warm-hold would remove, and\n  \
                     what it would cost the privacy property in crates/outloud/src/mic.rs.",
                    ms(tone_p50) - ms(warm)
                );
            }
            Err(e) => println!("\n  (warm comparison unavailable: {e})"),
        }
    }
}

/// Time a cold open *while another stream is already open on the same device*,
/// which is what the device looks like if the daemon kept it warm.
///
/// The second stream is what makes this honest: it measures the same
/// open-a-stream path as the cold runs, differing only in whether the hardware
/// was already running. Reusing an existing stream instead would measure
/// nothing at all and would always return zero.
fn warm_open(device: &cpal::Device, threshold: f32) -> anyhow::Result<Duration> {
    let config = device.default_input_config()?;
    let keeper = device.build_input_stream(
        config.into(),
        move |_data: &[f32], _: &cpal::InputCallbackInfo| {},
        |_e| {},
        None,
    )?;
    keeper.play()?;
    // Let the keeper get past the very startup being measured, so the device is
    // genuinely warm rather than mid-transition.
    std::thread::sleep(Duration::from_millis(800));

    let mut samples = Vec::new();
    for _ in 0..3 {
        if let Ok(p) = time_open(device, Some(threshold)) {
            if let Some(t) = p.live_tone {
                samples.push(t);
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    drop(keeper);

    if samples.is_empty() {
        anyhow::bail!("no warm run heard the tone");
    }
    Ok(percentile(&samples, 0.5))
}

/// What calibration concluded for one device.
struct Calibration {
    /// Median magnitude at the tone frequency while the tone plays.
    tone_level: f32,
    /// Median magnitude at the same frequency with the tone muted: this room,
    /// this device, no reference. The A/B is the whole point, because a device
    /// with a noisy front end can produce a large number at 1kHz that has
    /// nothing to do with the speakers.
    room_level: f32,
    threshold: f32,
}

/// Learn what the tone looks like on `device`, by listening twice: once with
/// the tone muted, once with it playing.
///
/// Rejects rather than guesses when the tone is not clearly above the room:
/// the whole point is to avoid the flapping-detector result described in the
/// module docs.
fn calibrate(device: &cpal::Device, tone: &Tone) -> anyhow::Result<Calibration> {
    tone.set_muted(true);
    // The output stream is a couple of buffers deep, so the muting takes a
    // moment to reach the speaker. Listening through that would put tone into
    // the "room" reading, which is the error this two-phase design exists to
    // remove.
    std::thread::sleep(Duration::from_millis(300));
    let room_level = steady_tone_level(device)?;

    tone.set_muted(false);
    std::thread::sleep(Duration::from_millis(300));
    let tone_level = steady_tone_level(device)?;

    if tone_level < room_level * MIN_TONE_OVER_ROOM {
        anyhow::bail!(
            "tone ({tone_level:.6}) is not clearly above this room ({room_level:.6}) \
             -> turn the output volume up, make sure the input is not muted, and \
             keep the device near the speakers"
        );
    }
    Ok(Calibration {
        tone_level,
        room_level,
        // Geometric midpoint between room and tone, so the detector sits as far
        // from a false positive on room noise as from a false negative on the
        // tone's own ripple. An arithmetic midpoint would hug the tone when the
        // two are orders of magnitude apart, which is the usual case.
        threshold: (tone_level * room_level).sqrt(),
    })
}

/// Hold `device` open for the calibration window and return the median
/// magnitude at the tone frequency, once the open transient has passed.
fn steady_tone_level(device: &cpal::Device) -> anyhow::Result<f32> {
    let config = device.default_input_config()?;
    let channels = config.channels() as usize;
    let sample_rate: u32 = config.sample_rate();

    let levels = Arc::new(Mutex::new(Vec::<f32>::new()));
    let sink = Arc::clone(&levels);
    let stream = device.build_input_stream(
        config.into(),
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let m = goertzel(data, channels, sample_rate, TONE_HZ);
            // A lock in an audio callback breaks the rule the ring buffer
            // follows, so this uses try_lock and drops the reading rather than
            // ever blocking the realtime thread. Acceptable here because this
            // is a diagnostic and a dropped sample only widens the percentile
            // spread slightly; the lock is uncontended for the whole window
            // anyway, since the reader waits until the stream is dropped.
            if let Ok(mut v) = sink.try_lock() {
                v.push(m);
            }
        },
        |_e| {},
        None,
    )?;
    stream.play()?;
    std::thread::sleep(CALIBRATION_WINDOW);
    drop(stream);

    let mut v = levels.lock().expect("calibration lock poisoned").clone();
    if v.len() < 10 {
        anyhow::bail!(
            "only {} buffers during calibration; device is not delivering audio",
            v.len()
        );
    }
    // Drop the first quarter: it holds the input filter's settling transient
    // after the open, which is not a steady-state reading of anything.
    v.drain(..v.len() / 4);
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(v[v.len() / 2])
}

/// Every phase of one cold open, all measured from the same T0.
///
/// Cumulative rather than per-phase deltas, because the question is always
/// "how long after the keypress", and a reader can subtract.
struct Phases {
    enumerate: Duration,
    config: Duration,
    build: Duration,
    play: Duration,
    first_buffer: Duration,
    /// True when the first buffer contained the tone but the buffers right
    /// after it did not. Live audio does not blink out for a single buffer, so
    /// this shape means buffer 1 held audio from before the open.
    stale_prefill: bool,
    /// First moment a sustained run of buffers contained the tone. `None` when
    /// that never happened before the timeout, or when there was no calibrated
    /// threshold to test against.
    live_tone: Option<Duration>,
}

/// Buffers that must all contain the tone before we call it live.
///
/// One buffer is not enough: the stale prefill is exactly one buffer. Five at
/// ~10.7ms each is ~53ms of continuous evidence, short enough not to inflate
/// the reported latency much and long enough that no artifact survives it.
const SUSTAIN_BUFFERS: usize = 5;

/// Open `device` cold and time each phase through to first real audio.
fn time_open(device: &cpal::Device, threshold: Option<f32>) -> anyhow::Result<Phases> {
    let t0 = Instant::now();

    // Touching the description forces the same CoreAudio property reads the
    // daemon's enumeration does, so this phase is not free by omission.
    let _ = device.description();
    let enumerate = t0.elapsed();

    // Deliberately the same call the shipped capture path makes
    // (crates/audio/src/capture_cpal.rs:198). Requesting a rate instead would
    // be simpler and would reconfigure a device another app is already using,
    // which crates/audio/tests/shared_device.rs exists to prevent.
    let config = device.default_input_config()?;
    let channels = config.channels() as usize;
    let sample_rate: u32 = config.sample_rate();
    let config_at = t0.elapsed();

    // Every buffer's arrival time and tone magnitude, decided afterwards on the
    // main thread. An earlier version made the "is it the tone" decision inside
    // the callback and recorded only the first hit, which is precisely how the
    // stale-prefill artifact went unnoticed: by the time the data reached a
    // human it had already been reduced to one number.
    let log: Arc<Mutex<Vec<(Duration, f32)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    let died = Arc::new(AtomicBool::new(false));
    let died_sink = Arc::clone(&died);
    let seen = Arc::new(AtomicU64::new(0));
    let seen_cb = Arc::clone(&seen);

    let stream = device.build_input_stream(
        config.into(),
        move |data: &[f32], _info: &cpal::InputCallbackInfo| {
            let at = t0.elapsed();
            let mag = match threshold {
                Some(_) => goertzel(data, channels, sample_rate, TONE_HZ),
                None => 0.0,
            };
            // try_lock, never lock: this is a realtime audio thread and must not
            // block on a reader. The reader only takes the lock after the stream
            // is dropped, so in practice this never contends.
            if let Ok(mut v) = sink.try_lock() {
                v.push((at, mag));
            }
            seen_cb.fetch_add(1, Ordering::SeqCst);
        },
        move |_e| died_sink.store(true, Ordering::SeqCst),
        None,
    )?;
    let build = t0.elapsed();

    stream.play()?;
    let play = t0.elapsed();

    // Keep listening until the tone has been sustained, not merely glimpsed: a
    // device can deliver punctual nothing (or one stale buffer) for a long time
    // before its capture chain is really running, and that interval is the
    // entire point of this example.
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if died.load(Ordering::SeqCst) {
            anyhow::bail!("stream errored during open");
        }
        match threshold {
            None => {
                if seen.load(Ordering::SeqCst) > 0 {
                    break;
                }
            }
            Some(t) => {
                if let Ok(v) = log.try_lock() {
                    if sustained_from(&v, t).is_some() {
                        break;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(stream);

    let buffers = log.lock().expect("log lock poisoned").clone();
    let Some(&(first_buffer, first_mag)) = buffers.first() else {
        anyhow::bail!(
            "no buffer within {TIMEOUT:?} ({sample_rate}Hz, {channels}ch); \
             is microphone permission granted?"
        );
    };

    let (stale_prefill, live_tone) = match threshold {
        None => (false, None),
        Some(t) => (
            first_mag > t && buffers.get(1).map(|(_, m)| *m <= t).unwrap_or(false),
            sustained_from(&buffers, t),
        ),
    };

    Ok(Phases {
        enumerate,
        config: config_at,
        build,
        play,
        first_buffer,
        stale_prefill,
        live_tone,
    })
}

/// Arrival time of the first buffer beginning a run of [`SUSTAIN_BUFFERS`]
/// consecutive buffers all above `threshold`.
fn sustained_from(buffers: &[(Duration, f32)], threshold: f32) -> Option<Duration> {
    buffers
        .windows(SUSTAIN_BUFFERS)
        .find(|w| w.iter().all(|(_, m)| *m > threshold))
        .map(|w| w[0].0)
}

/// Energy at one frequency, normalized per sample, via the Goertzel algorithm.
///
/// A whole FFT would answer a question nobody asked. Goertzel is the single-bin
/// case: one multiply-add per sample, no allocation, which matters because this
/// runs inside a realtime audio callback.
///
/// Interleaved input is folded to mono the same way `resample::downmix` does,
/// so a multi-channel device is scored on what it heard overall rather than on
/// whichever channel happened to be first.
fn goertzel(interleaved: &[f32], channels: usize, sample_rate: u32, freq: f32) -> f32 {
    let ch = channels.max(1);
    let frames = interleaved.len() / ch;
    if frames == 0 {
        return 0.0;
    }
    let coeff = 2.0 * (2.0 * std::f32::consts::PI * freq / sample_rate as f32).cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for frame in interleaved.chunks_exact(ch) {
        let x = frame.iter().sum::<f32>() / ch as f32;
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    // Magnitude of the bin, divided by the frame count so the threshold does
    // not depend on the device's buffer size.
    (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0).sqrt() / frames as f32
}

/// A continuous reference tone on the default output device, alive for as long
/// as the returned handle is.
///
/// Mutable rather than merely on, because calibration has to hear the room
/// *without* the tone to know whether the tone is really audible on a given
/// device. Muting is a flag the output callback reads, not a stop and restart
/// of the stream: restarting would reconfigure the output device mid-sweep,
/// which is a change to the thing being measured.
struct Tone {
    device_name: String,
    muted: Arc<AtomicBool>,
    _stream: cpal::Stream,
}

impl Tone {
    fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::SeqCst);
    }
}

fn start_tone(host: &cpal::Host) -> anyhow::Result<Tone> {
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no default output device"))?;
    let device_name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "<unnamed output>".into());
    let config = device.default_output_config()?;
    let channels = config.channels() as usize;
    let sample_rate: u32 = config.sample_rate();

    // Phase carried across callbacks, in the callback's own state: a tone that
    // restarted its phase every buffer would click, and clicks are broadband,
    // which would let a device "hear the tone" from the discontinuity alone.
    // Phase keeps advancing while muted for the same reason: unmuting mid-cycle
    // must not produce a step.
    let mut phase = 0.0f32;
    let step = 2.0 * std::f32::consts::PI * TONE_HZ / sample_rate as f32;
    let muted = Arc::new(AtomicBool::new(false));
    let muted_cb = Arc::clone(&muted);

    let stream = device.build_output_stream(
        config.into(),
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let gain = if muted_cb.load(Ordering::Relaxed) {
                0.0
            } else {
                TONE_AMPLITUDE
            };
            for frame in data.chunks_mut(channels.max(1)) {
                let s = gain * phase.sin();
                phase = (phase + step) % (2.0 * std::f32::consts::PI);
                for slot in frame.iter_mut() {
                    *slot = s;
                }
            }
        },
        |e| eprintln!("tone output error: {e}"),
        None,
    )?;
    stream.play()?;
    Ok(Tone {
        device_name,
        muted,
        _stream: stream,
    })
}

/// The segmenter's pre-roll, read from the config rather than restated, so this
/// cannot drift from the value that actually ships.
fn pre_roll_ms() -> usize {
    audio::segment::SegmenterConfig::default().pre_roll_frames * 30
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn percentile(values: &[Duration], q: f64) -> Duration {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn summarize(label: &str, values: &[Duration]) {
    let mut sorted = values.to_vec();
    sorted.sort();
    println!(
        "  {label}: min={:.1}ms  p50={:.1}ms  p90={:.1}ms  max={:.1}ms",
        ms(sorted[0]),
        ms(percentile(&sorted, 0.5)),
        ms(percentile(&sorted, 0.9)),
        ms(*sorted.last().unwrap())
    );
}
