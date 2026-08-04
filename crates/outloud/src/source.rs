//! Pipeline frontends: where control edges and audio come from.
//!
//! Three producers feed one unified event channel:
//!
//! - [`spawn_hotkey`]: bridges the CGEventTap's std-mpsc events into the
//!   tokio world on a dedicated thread. The tap callback itself never sees
//!   tokio; it pushes into an unbounded std channel and returns (the hard
//!   rule from crates/hotkey), and this bridge does the slow half.
//! - [`spawn_mic`]: cpal capture into the ring buffer, drained on a 30ms
//!   tick. The ring drops oldest under overrun, so a stalled consumer costs
//!   audio, never capture-thread time.
//! - [`spawn_wav`]: replays a WAV file as one synthetic utterance
//!   (KeyDown, chunks, KeyUp), so `--once --wav` exercises every stage
//!   downstream of capture on machines with no usable microphone.
//!
//! All three send [`FrontendEvent`]s; the supervisor in [`crate::pipeline`]
//! is the only consumer and cannot tell which frontend is attached, which is
//! what makes the file-driven test mode an honest test of the real path.

use tokio::sync::mpsc::UnboundedSender;

use crate::runtime::RuntimeShared;

/// Ring-drain cadence, matching the 30ms VAD frame size so each tick hands
/// the segmenter roughly one frame's worth of audio.
///
/// Gated with the capture path it belongs to: only `spawn_mic` drains a
/// ring, so a headless build would carry this as dead code and fail
/// `clippy -D warnings`.
#[cfg(feature = "display")]
const DRAIN_TICK_MS: u64 = 30;

/// Everything the supervisor reacts to.
#[derive(Debug)]
pub enum FrontendEvent {
    /// Hotkey went down (or a synthetic utterance began): start listening.
    KeyDown,
    /// Hotkey released / unlatched (or synthetic utterance ended): commit.
    KeyUp,
    /// 16kHz mono audio. Flows continuously from the mic frontend; the
    /// supervisor discards it while not listening (the mic is only *used*
    /// while the key is held, matching the push-to-talk trust story).
    Chunk(Vec<f32>),
    /// Capture-side lifecycle worth surfacing (device died, no mic).
    CaptureIssue(String),
    /// Capture recovered (stream up on a device).
    CaptureUp(String),
}

/// Bind the global hotkey and bridge its events. Returns the bound chord's
/// display string, or the error the caller must surface as NoPermission /
/// Error state (a dead hotkey must be loud, per the UX doc).
pub fn spawn_hotkey(
    chord: hotkey::Chord,
    tx: UnboundedSender<FrontendEvent>,
    runtime: RuntimeShared,
) -> Result<String, hotkey::HotkeyError> {
    let manager = hotkey::HotkeyManager::bind(chord.clone(), hotkey::Timing::default())?;
    for c in manager.conflicts() {
        // Advisory per the UX doc: warn loudly, never silently accept.
        eprintln!("outloud: hotkey conflict: {c:?}");
    }
    // Publish what was ACTUALLY bound, not what was asked for. The menu bar
    // shows this, and a menu that echoes the config file would hide exactly
    // the case a user needs to see: the file says one thing, the live tap
    // another.
    runtime.set_bound_hotkey(Some(chord.to_string()));
    std::thread::Builder::new()
        .name("outloud-hotkey-bridge".into())
        .spawn(move || {
            // recv() blocks on the std channel; this thread exists so the
            // tokio reactor never does.
            while let Ok(ev) = manager.events().recv() {
                use hotkey::HotkeyEvent::*;
                let mapped = match ev {
                    Pressed => Some(FrontendEvent::KeyDown),
                    // Released ends a hold; Unlatched ends a tap-latched
                    // capture. Same commit semantics downstream.
                    Released | Unlatched => Some(FrontendEvent::KeyUp),
                    // Latched: capture simply continues; nothing to emit.
                    Latched => None,
                    TapRecovered => {
                        eprintln!("outloud: event tap was disabled and recovered");
                        None
                    }
                };
                // Paused (`enabled = false`, the menu's Pause row): drop the
                // edge here, before anything downstream starts listening.
                // Dropping it at the source is what makes "paused" mean the
                // microphone is never opened, rather than "recorded and then
                // thrown away" — a distinction the whole trust story rests
                // on. KeyUp still passes so a pause mid-utterance commits
                // what was already captured instead of stranding it.
                if !runtime.enabled() && matches!(mapped, Some(FrontendEvent::KeyDown)) {
                    continue;
                }
                if let Some(m) = mapped {
                    if tx.send(m).is_err() {
                        return; // supervisor gone
                    }
                }
            }
        })
        .expect("spawning hotkey bridge thread");
    Ok(chord.to_string())
}

/// Start microphone capture and the ring-drain task. The returned handle
/// keeps the capture supervisor thread alive; drop it to stop.
///
/// Only compiled when the `audio/capture` backend is present. A headless
/// build has no audio stack linked at all (that is the point: it must not
/// drag ALSA onto a server), so the counterpart below fails with an error
/// that names the flag to use instead.
#[cfg(feature = "display")]
pub fn spawn_mic(
    tx: UnboundedSender<FrontendEvent>,
    runtime: RuntimeShared,
) -> anyhow::Result<audio::capture::CaptureHandle> {
    // 10 seconds of ring: deep enough that only a genuinely wedged drain
    // loses audio, and losses are counted, not silent.
    let (producer, consumer) = audio::ring::ring(audio::SAMPLE_RATE as usize * 10);

    let tx_events = tx.clone();
    let handle = audio::capture::start_capture(producer, move |ev| {
        use audio::capture::CaptureEvent::*;
        let mapped = match ev {
            Started { device } => {
                // Which microphone actually won. "Granted, but recording
                // from the wrong device" is otherwise undiagnosable: it
                // presents identically to a broken microphone.
                runtime.set_microphone(device.clone());
                FrontendEvent::CaptureUp(device)
            }
            DeviceChanged { from } => FrontendEvent::CaptureIssue(format!(
                "input device changed (was {from}); rebuilding stream"
            )),
            Error { message } => {
                // Only a total absence of input is a "go fix the microphone
                // permission" situation; a transient stream error self-heals
                // via the rebuild loop and must not raise a menu row.
                if message.contains("no input device") {
                    runtime.set_microphone_blocked();
                }
                FrontendEvent::CaptureIssue(message)
            }
        };
        let _ = tx_events.send(mapped);
    });

    // Drain the ring on a steady tick. 30ms matches the VAD frame size, so
    // each tick hands the segmenter roughly one frame.
    //
    // The task must STOP when this capture stops. The microphone is now
    // opened per-utterance, so a drain task that outlived its stream would
    // accumulate one leaked task per dictation, each holding a consumer for
    // a ring nothing writes to any more. `stopped` is the same flag the
    // capture handle flips, so the two die together by construction rather
    // than by remembering to cancel.
    let stopped = handle.stop_flag();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(DRAIN_TICK_MS));
        // A missed tick (system sleep) should catch up by draining more,
        // not by firing a burst of empty polls.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut buf = vec![0f32; audio::SAMPLE_RATE as usize]; // up to 1s per tick
        loop {
            interval.tick().await;
            if !stopped.load(std::sync::atomic::Ordering::SeqCst) {
                return; // this capture is over; its ring is dead
            }
            let n = consumer.pop(&mut buf);
            if n > 0 && tx.send(FrontendEvent::Chunk(buf[..n].to_vec())).is_err() {
                return; // supervisor gone: stop draining
            }
        }
    });
    Ok(handle)
}

/// Headless counterpart: there is no capture backend in this build.
///
/// Returns an error rather than panicking or silently producing no audio,
/// because "the daemon started and then never heard anything" is the single
/// most confusing failure this program can present. The message names
/// `--wav` because that is the path that actually works here, and an error
/// that only says no is an error the user cannot act on.
#[cfg(not(feature = "display"))]
pub fn spawn_mic(
    _tx: UnboundedSender<FrontendEvent>,
    _runtime: RuntimeShared,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "this build has no microphone support (built without the `display` \
         feature, so no audio capture backend is linked) -> feed audio from a \
         file with `--once --wav FILE`, or use a build with default features"
    )
}

/// Replay `samples` (16kHz mono) as one utterance: KeyDown, real-time-ish
/// chunks, KeyUp. `realtime` pacing exercises streaming partials the way a
/// human would; `false` shoves it through as fast as the channel takes it,
/// for CI where wall time matters.
pub fn spawn_wav(samples: Vec<f32>, realtime: bool, tx: UnboundedSender<FrontendEvent>) {
    spawn_wav_sequence(vec![samples], realtime, tx)
}

/// Replay SEVERAL utterances through one process, as separate key cycles.
///
/// Anything spanning utterances cannot be tested with one recording per
/// process. Undo is the case that forced this: the ring is process-lifetime
/// (the dictation being undone finished before the one asking for the undo
/// began), so a `--once` run's ring is always empty and "scratch that" can
/// never succeed there. A verification built on single runs would have
/// reported success while never exercising undo at all.
///
/// Each utterance is a full KeyDown/chunks/KeyUp cycle with a gap between,
/// so the pipeline commits one before the next begins, exactly as a person
/// speaking twice would produce.
pub fn spawn_wav_sequence(
    utterances: Vec<Vec<f32>>,
    realtime: bool,
    tx: UnboundedSender<FrontendEvent>,
) {
    tokio::spawn(async move {
        // OUTLOUD_REPLAY_DELAY_MS: wait before the first key cycle so the
        // window under test can be focused first.
        //
        // Dictate-vs-edit is decided at KEY-DOWN, from whatever is focused
        // then. A replay launched from a console therefore always sees the
        // console: it reports Dictate, and the edit and undo routes cannot be
        // reached at all. That is not a hypothetical -- it is why undo stayed
        // unverifiable on Windows, where every check is run from a terminal.
        // Delaying the first key-down leaves a window for a fixture (or a
        // person) to focus the real target, so the routing under test is the
        // routing that runs in production.
        if let Some(ms) = std::env::var("OUTLOUD_REPLAY_DELAY_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            eprintln!("outloud: waiting {ms}ms before replaying, focus your target now (test knob)");
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
        for (i, samples) in utterances.into_iter().enumerate() {
            if i > 0 {
                // Long enough for the previous utterance to commit and its
                // write to land before the next key cycle opens. Without
                // this the second utterance races the first one's delivery
                // and reads a field that is still mid-write.
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            }
            let _ = tx.send(FrontendEvent::KeyDown);
            // 200ms chunks: the pacing the asr benchmarks use, coarse enough
            // to be cheap, fine enough that partials interleave.
            let chunk = audio::SAMPLE_RATE as usize / 5;
            for c in samples.chunks(chunk) {
                if tx.send(FrontendEvent::Chunk(c.to_vec())).is_err() {
                    return;
                }
                if realtime {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
            let _ = tx.send(FrontendEvent::KeyUp);
        }
    });
}
