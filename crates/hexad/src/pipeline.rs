//! The supervisor: one async task that owns the product state machine and
//! wires every stage together (deliverable 1).
//!
//! Event-driven, no polling except the ring drain in [`crate::source`]. The
//! loop `select!`s over the frontend channel (hotkey edges + audio) and the
//! recognizer channel (partials + finals). All blocking work lives
//! elsewhere: recognition on its own thread, injection in `spawn_blocking`,
//! so a hung target application can stall at most one utterance's write,
//! never the hotkey or the overlay.

use std::time::Instant;

use diag::timing::{Recorder, Stage};
use overlay::OverlayState;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::inject::{self, Mode, Outcome};
use crate::recognize::{AsrEvent, AudioFeed};
use crate::source::FrontendEvent;
use crate::state::Engine;

/// How long an error panel stays on screen before dismissing itself.
///
/// Long enough to read one line, short enough that the daemon visibly
/// returns to normal rather than looking wedged. Errors here are advisory:
/// nothing is lost by clearing one, and the message is also on stderr.
const ERROR_DISMISS_AFTER: std::time::Duration = std::time::Duration::from_secs(4);

/// Supervisor behaviour knobs.
pub struct Config {
    /// Exit after one committed utterance (the `--once` testing mode).
    pub once: bool,
    /// Commit on the VAD endpoint instead of waiting for a key edge. This is
    /// what lets `--once` run a full cycle with nobody holding a key: speak,
    /// pause, and the hangover fires the commit.
    pub auto_endpoint: bool,
}

/// What one utterance actually cost, measured where the user feels it.
#[derive(Debug, Clone)]
pub struct UtteranceReport {
    pub transcript: String,
    /// How the text landed ("set-value", "clipboard-paste", ...), or why it
    /// did not.
    pub outcome: String,
    /// Key release (or VAD endpoint) to recognizer final.
    pub finalize_ms: f64,
    /// Intent parse + apply + transport write.
    pub inject_ms: f64,
    /// THE number: key release to text on screen.
    pub release_to_text_ms: f64,
    /// Audio chunks dropped because the recognizer fell behind. Zero in
    /// health; nonzero is reported, never hidden.
    pub dropped_chunks: u64,
}

impl UtteranceReport {
    pub fn render(&self) -> String {
        format!(
            "e2e: release->text {:.0}ms (finalize {:.0}ms, inject {:.1}ms) via {} | \"{}\"{}",
            self.release_to_text_ms,
            self.finalize_ms,
            self.inject_ms,
            self.outcome,
            self.transcript,
            if self.dropped_chunks > 0 {
                format!(" | DROPPED {} audio chunks", self.dropped_chunks)
            } else {
                String::new()
            }
        )
    }
}

/// Per-utterance bookkeeping while an utterance is in flight.
struct InFlight {
    mode: Mode,
    released_at: Instant,
}

/// Run the supervisor until the frontend channel closes, or (in `once`
/// mode) until the first utterance commits. Returns the reports of every
/// committed utterance, plus a percentile summary via `recorder`.
/// The wiring one run needs: where events arrive from and where audio goes.
///
/// Grouped into a struct rather than passed as loose parameters because
/// they are one thing (the pipeline's plumbing) and they are always
/// constructed together. It also keeps `run` under the argument-count lint,
/// which was the nudge that made the grouping obvious.
pub struct Channels {
    pub frontend: UnboundedReceiver<FrontendEvent>,
    pub asr_events: UnboundedReceiver<AsrEvent>,
    pub feed: AudioFeed,
    pub ready: tokio::sync::oneshot::Receiver<anyhow::Result<&'static str>>,
    /// The microphone, when this run owns one. `None` for file-driven and
    /// test runs, which supply their own samples and must never open a
    /// device.
    pub mic: Option<crate::mic::Mic>,
}

pub async fn run(
    cfg: Config,
    mut engine: Engine,
    channels: Channels,
    recorder: &mut Recorder,
) -> anyhow::Result<Vec<UtteranceReport>> {
    let Channels {
        mut frontend,
        mut asr_events,
        feed,
        ready,
        mut mic,
    } = channels;
    let mut reports = Vec::new();
    let mut segmenter = new_segmenter();
    let mut listening = false;
    // Key held before the recognizer was ready: the model-loading state
    // buffers the capture instead of losing the user's words (UX doc).
    let mut pending_listen = false;
    let mut in_flight: Option<InFlight> = None;
    let mut ready = std::pin::pin!(ready);
    let mut recognizer_ready = false;
    // The asr channel closing is only terminal once readiness resolved:
    // a recognizer that fails to construct drops its sender BEFORE the
    // ready result is polled, and that race must surface the load error,
    // not an empty success.
    let mut asr_closed = false;

    loop {
        tokio::select! {
            // Biased: readiness resolves before any Final that raced past
            // it (the worker sends `ready` before touching audio, but the
            // random-order select could still poll the channels first).
            biased;

            // Tick only while an error is on screen, so the idle daemon is
            // still fully event-driven. An error panel that only clears on
            // the next key-down is indistinguishable from a hang.
            _ = tokio::time::sleep(ERROR_DISMISS_AFTER), if engine.state() == OverlayState::Error => {
                engine.dismiss_stale_error(ERROR_DISMISS_AFTER);
            }

            // Recognizer readiness: ModelLoading -> Idle, or a named failure.
            r = &mut ready, if !recognizer_ready => {
                recognizer_ready = true;
                match r {
                    Ok(Ok(name)) => {
                        eprintln!("hexad: recognizer ready: {name}");
                        engine.transition(OverlayState::Idle, None);
                        if pending_listen {
                            pending_listen = false;
                            if listening {
                                // Key still held: buffered capture becomes
                                // live listening.
                                engine.transition(OverlayState::Listening, None);
                            } else if in_flight.is_some() {
                                // The whole utterance happened during model
                                // load (key already released). Walk the
                                // diagram's edges to Transcribing: the
                                // finalize is already queued behind the
                                // buffered audio.
                                engine.transition(OverlayState::Listening, None);
                                engine.transition(OverlayState::Transcribing, None);
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        let msg = format!(
                            "recognizer failed to load ({e}) -> build the speech helper \
                             (see crates/asr/helper) or run with --asr mock"
                        );
                        engine.transition(OverlayState::Error, Some(msg.clone()));
                        anyhow::bail!("{msg}");
                    }
                    Err(_) => anyhow::bail!("recognizer thread vanished before becoming ready"),
                }
            }

            ev = frontend.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    FrontendEvent::KeyDown => {
                        if in_flight.is_some() {
                            // Utterance still finalizing: refuse overlap. A
                            // queued double-capture would interleave two
                            // recognizer utterances on one channel.
                            eprintln!("hexad: key-down ignored, previous utterance still committing");
                            continue;
                        }
                        // Error-shaped states exit through Idle on the next
                        // interaction: the named exit the UX doc requires.
                        if matches!(
                            engine.state(),
                            OverlayState::Error | OverlayState::NoPermission | OverlayState::DegradedOffline
                        ) {
                            engine.transition(OverlayState::Idle, None);
                        }
                        // Snapshot at KEY-DOWN decides dictate-vs-edit: it
                        // captures the selection the user is looking at while
                        // they speak. Warm AX read is ~134us (docs/latency.md),
                        // cheap enough for the event loop.
                        let span = recorder.start(Stage::Read);
                        let mode = inject::mode_at_keydown();
                        recorder.finish(span);
                        if let Mode::Edit { selected } = &mode {
                            eprintln!("hexad: edit mode on selection: \"{selected}\"");
                        }
                        segmenter = new_segmenter();
                        // Open the device HERE, not at startup. Holding a
                        // stream open all session lights the system's
                        // recording indicator permanently, which tells the
                        // user they are being recorded while idle. See
                        // crates/hexad/src/mic.rs.
                        if let Some(m) = mic.as_mut() {
                            if let Err(e) = m.open() {
                                eprintln!("hexad: could not open the microphone: {e}");
                                engine.transition(
                                    OverlayState::Error,
                                    Some("could not open the microphone -> check Privacy settings".into()),
                                );
                                continue;
                            }
                        }
                        in_flight = Some(InFlight { mode, released_at: Instant::now() });
                        if recognizer_ready {
                            listening = true;
                            engine.transition(OverlayState::Listening, None);
                        } else {
                            // Buffered capture during model load: audio flows
                            // into the recognizer channel and waits there.
                            pending_listen = true;
                            listening = true;
                            engine.transition(
                                OverlayState::ModelLoading,
                                Some("will transcribe when the model is ready".into()),
                            );
                        }
                    }
                    FrontendEvent::KeyUp => {
                        if listening {
                            commit(&mut engine, &mut segmenter, &feed, &mut in_flight, &mut listening);
                            // Release the device as soon as the user stops
                            // speaking, so the recording indicator tracks
                            // dictation rather than uptime.
                            if let Some(m) = mic.as_mut() {
                                m.close();
                            }
                        }
                    }
                    FrontendEvent::Chunk(samples) => {
                        if !listening {
                            continue; // mic is only *used* while capturing
                        }
                        engine.live(level_of(&samples), None);
                        let mut endpoint = false;
                        for ev in segmenter.push(&samples) {
                            use audio::segment::SpeechEvent::*;
                            match ev {
                                // Stream utterance audio to the recognizer as
                                // it arrives; drop-not-block inside push().
                                SpeechStart { audio } | Partial { audio } => feed.push(audio),
                                // The full audio was already streamed above;
                                // feeding it again would duplicate words.
                                SpeechEnd { .. } => endpoint = true,
                            }
                        }
                        if endpoint && cfg.auto_endpoint {
                            commit(&mut engine, &mut segmenter, &feed, &mut in_flight, &mut listening);
                            // Same release as the key-up path: every route
                            // out of listening must close the device, or a
                            // silence-committed utterance leaves it open.
                            if let Some(m) = mic.as_mut() {
                                m.close();
                            }
                        }
                    }
                    FrontendEvent::CaptureUp(device) => {
                        eprintln!("hexad: capturing from {device}");
                    }
                    FrontendEvent::CaptureIssue(msg) => {
                        // Capture self-heals (rebuild loop); a total absence of
                        // input is the one unrecoverable case worth the Error
                        // state, with the next action named.
                        eprintln!("hexad: capture: {msg}");
                        if msg.contains("no input device") && engine.state() == OverlayState::Idle {
                            engine.transition(
                                OverlayState::Error,
                                Some("no microphone -> connect one, or test with --once --wav".into()),
                            );
                        }
                    }
                }
            }

            ev = asr_events.recv(), if !asr_closed => {
                let Some(ev) = ev else {
                    asr_closed = true;
                    if recognizer_ready {
                        break;
                    }
                    continue;
                };
                match ev {
                    AsrEvent::Partial(text) => {
                        // Ghost text in the overlay only; the field is
                        // touched exactly once, at commit (commit-on-release).
                        engine.live(0.0, Some(&text));
                    }
                    AsrEvent::Final(result) => {
                        let Some(fl) = in_flight.take() else {
                            eprintln!("hexad: stray final transcript ignored");
                            continue;
                        };
                        let finalize_ms = fl.released_at.elapsed().as_secs_f64() * 1000.0;
                        match result {
                            Ok(t) => {
                                let report = commit_transcript(
                                    &mut engine, &fl, t.text.trim(), finalize_ms, &feed, recorder,
                                ).await;
                                if let Some(r) = report {
                                    eprintln!("hexad: {}", r.render());
                                    reports.push(r);
                                }
                            }
                            Err(e) => {
                                engine.transition(
                                    OverlayState::Error,
                                    Some(format!("recognizer fault ({e}) -> try again; if it persists, rebuild the speech helper")),
                                );
                            }
                        }
                        if cfg.once {
                            return Ok(reports);
                        }
                    }
                }
            }
        }
    }
    Ok(reports)
}

/// End the capture half of an utterance: flush the segmenter, tell the
/// recognizer to finalize, stamp the release time the e2e number is
/// measured from.
fn commit(
    engine: &mut Engine,
    segmenter: &mut Segmenter,
    feed: &AudioFeed,
    in_flight: &mut Option<InFlight>,
    listening: &mut bool,
) {
    *listening = false;
    // Audio inside the segmenter's onset debounce would otherwise be lost;
    // flush emits it as a final SpeechEnd whose audio was NOT yet streamed
    // only in the never-triggered case, which yields an empty transcript
    // and the quiet Idle return. Streamed frames are not re-sent.
    let _ = segmenter.flush();
    if let Some(fl) = in_flight.as_mut() {
        fl.released_at = Instant::now();
    }
    feed.finalize();
    // During model load the state stays ModelLoading (buffered capture);
    // the ready handler walks the Listening -> Transcribing edges then.
    if engine.state() == OverlayState::Listening {
        engine.transition(OverlayState::Transcribing, None);
    }
}

/// The back half: transcript -> (parse -> apply ->) write -> state updates.
/// Returns `None` for the quiet empty-transcript path.
async fn commit_transcript(
    engine: &mut Engine,
    fl: &InFlight,
    text: &str,
    finalize_ms: f64,
    feed: &AudioFeed,
    recorder: &mut Recorder,
) -> Option<UtteranceReport> {
    if text.is_empty() {
        // The documented "empty (silence) result" edge.
        engine.transition(OverlayState::Idle, None);
        return None;
    }
    engine.transition(OverlayState::Injecting, None);

    let mode = fl.mode.clone();
    let owned = text.to_string();
    let inject_started = Instant::now();
    // AX writes are ~13ms of synchronous IPC into another process, and a
    // wedged target can take much longer: off the event loop it goes.
    let outcome = tokio::task::spawn_blocking(move || inject::deliver(&mode, &owned))
        .await
        .unwrap_or_else(|e| Outcome::Failed {
            situation_action: format!("injection task panicked ({e}) -> please file a bug"),
        });
    let inject_ms = inject_started.elapsed().as_secs_f64() * 1000.0;
    recorder.record(
        Stage::Write,
        std::time::Duration::from_secs_f64(inject_ms / 1000.0),
    );

    let outcome_str = match outcome {
        Outcome::Wrote { via, .. } => {
            engine.transition(OverlayState::Idle, None);
            via
        }
        Outcome::EmptyTranscript => {
            engine.transition(OverlayState::Idle, None);
            "empty".into()
        }
        Outcome::FreeformUnsupported { instruction } => {
            // Deliverable 3: freeform has no local LLM yet. Say so, name
            // what WAS heard and what to do instead. Never silent.
            let msg = format!(
                "freeform edit \"{instruction}\" needs the local LLM (not shipped yet) \
                 -> rephrase as change/replace/delete/add/case"
            );
            engine.transition(OverlayState::Error, Some(msg.clone()));
            format!("freeform-unsupported: {msg}")
        }
        Outcome::EditNoMatch { command } => {
            let msg =
                format!("\"{command}\" matched nothing in the selection -> check the exact words");
            engine.transition(OverlayState::Error, Some(msg.clone()));
            format!("edit-no-match: {msg}")
        }
        Outcome::Failed { situation_action } => {
            engine.transition(OverlayState::Error, Some(situation_action.clone()));
            format!("failed: {situation_action}")
        }
    };

    Some(UtteranceReport {
        transcript: text.to_string(),
        outcome: outcome_str,
        finalize_ms,
        inject_ms,
        release_to_text_ms: finalize_ms + inject_ms,
        dropped_chunks: feed.dropped_chunks(),
    })
}

type Segmenter = audio::segment::SpeechSegmenter<audio::vad::EnergyVad>;

/// A fresh per-utterance segmenter. Energy VAD: dependency-free and good
/// enough behind push-to-talk, where the key edges, not the VAD, bound the
/// utterance; swap for Silero (the `silero` feature in crates/audio) when
/// voice activation lands.
fn new_segmenter() -> Segmenter {
    audio::segment::SpeechSegmenter::new(
        audio::vad::EnergyVad::new(),
        audio::segment::SegmenterConfig::default(),
    )
}

/// Overlay waveform level: normalized RMS. The 0.1 knee maps normal speech
/// near full scale without clipping whispers to zero.
fn level_of(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    (rms / 0.1).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recognize;
    use crate::source::FrontendEvent;
    use asr::backends::mock::MockRecognizer;

    fn voiced(secs: f32) -> Vec<f32> {
        (0..(secs * 16_000.0) as usize)
            .map(|i| 0.3 * (i as f32 * 0.2).sin())
            .collect()
    }

    /// The full supervisor loop against the mock recognizer: a synthetic
    /// utterance must produce exactly one report with a nonempty transcript.
    /// Injection will fail (no focused field in CI) and must fail *named*,
    /// not panic: that is the graceful-degradation contract under test.
    #[tokio::test]
    async fn one_utterance_flows_end_to_end() {
        let (ftx, frx) = tokio::sync::mpsc::unbounded_channel();
        let (atx, arx) = tokio::sync::mpsc::unbounded_channel();
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        let feed = recognize::spawn(|| Ok(Box::new(MockRecognizer::new()) as _), atx, rtx);
        let (engine, shared) = Engine::new();
        let mut recorder = Recorder::new();

        ftx.send(FrontendEvent::KeyDown).unwrap();
        for chunk in voiced(3.0).chunks(1600) {
            ftx.send(FrontendEvent::Chunk(chunk.to_vec())).unwrap();
        }
        ftx.send(FrontendEvent::KeyUp).unwrap();

        let cfg = Config {
            once: true,
            auto_endpoint: false,
        };
        let reports = run(
            cfg,
            engine,
            Channels {
                frontend: frx,
                asr_events: arx,
                feed,
                ready: rrx,
                mic: None,
            },
            &mut recorder,
        )
        .await
        .unwrap();
        assert_eq!(reports.len(), 1);
        let r = &reports[0];
        assert!(!r.transcript.is_empty(), "voiced audio must transcribe");
        assert!(r.release_to_text_ms >= r.inject_ms);
        assert_eq!(r.dropped_chunks, 0);
        // In CI there is no focused text field; the outcome must be a named
        // degradation (clipboard fallback or failed-with-action), never
        // empty and never a panic.
        assert!(!r.outcome.is_empty());
        // Terminal state must be one the diagram allows to rest in.
        let final_state = shared.snapshot().state;
        assert!(
            matches!(final_state, OverlayState::Idle | OverlayState::Error),
            "ended in {final_state}"
        );
    }

    /// Silence in, nothing out: the empty-transcript path returns quietly to
    /// Idle with no report and no write.
    #[tokio::test]
    async fn silence_commits_nothing() {
        let (ftx, frx) = tokio::sync::mpsc::unbounded_channel();
        let (atx, arx) = tokio::sync::mpsc::unbounded_channel();
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        let feed = recognize::spawn(|| Ok(Box::new(MockRecognizer::new()) as _), atx, rtx);
        let (engine, shared) = Engine::new();
        let mut recorder = Recorder::new();

        ftx.send(FrontendEvent::KeyDown).unwrap();
        ftx.send(FrontendEvent::Chunk(vec![0.0; 16_000])).unwrap();
        ftx.send(FrontendEvent::KeyUp).unwrap();

        let cfg = Config {
            once: true,
            auto_endpoint: false,
        };
        let reports = run(
            cfg,
            engine,
            Channels {
                frontend: frx,
                asr_events: arx,
                feed,
                ready: rrx,
                mic: None,
            },
            &mut recorder,
        )
        .await
        .unwrap();
        assert!(reports.is_empty());
        assert_eq!(shared.snapshot().state, OverlayState::Idle);
    }

    /// A recognizer that cannot construct must surface as a named error,
    /// not a hang.
    #[tokio::test]
    async fn recognizer_load_failure_is_a_named_error() {
        let (_ftx, frx) = tokio::sync::mpsc::unbounded_channel();
        let (atx, arx) = tokio::sync::mpsc::unbounded_channel();
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        let feed = recognize::spawn(|| anyhow::bail!("no helper"), atx, rtx);
        let (engine, shared) = Engine::new();
        let mut recorder = Recorder::new();
        let cfg = Config {
            once: true,
            auto_endpoint: false,
        };
        let err = run(
            cfg,
            engine,
            Channels {
                frontend: frx,
                asr_events: arx,
                feed,
                ready: rrx,
                mic: None,
            },
            &mut recorder,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no helper"));
        assert_eq!(shared.snapshot().state, OverlayState::Error);
        // The error must carry a named next action.
        assert!(shared.snapshot().detail.unwrap().contains("->"));
    }
}
