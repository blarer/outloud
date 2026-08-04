//! The supervisor: one async task that owns the product state machine and
//! wires every stage together (deliverable 1).
//!
//! Event-driven, no polling except the ring drain in [`crate::source`]. The
//! loop `select!`s over the frontend channel (hotkey edges + audio) and the
//! recognizer channel (partials + finals). All blocking work lives
//! elsewhere: recognition on its own thread, injection in `spawn_blocking`,
//! so a hung target application can stall at most one utterance's write,
//! never the hotkey or the overlay.

use std::sync::Arc;
use std::time::Instant;

use diag::timing::{Recorder, Stage};
use overlay::OverlayState;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::devlatency::{StartupWatch, Verdict};
use crate::inject::{self, Mode, Outcome};
use crate::recognize::{AsrEvent, AudioFeed};
use crate::source::FrontendEvent;
use crate::state::Engine;
use crate::streamer::{Streamer, StreamerEvent};

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
    /// `insertion.mode = "stream"`: write stable partial prefixes into the
    /// field as they prove out, instead of one insert at commit. Only
    /// honoured when the focused field can take in-place revisions; every
    /// other field silently keeps commit-on-release (docs/streaming.md's
    /// degradation matrix).
    pub prefer_streaming: bool,
    /// `microphone.sensitivity` (1-100): how quiet a voice still counts as
    /// speech. Carried here rather than read at the VAD so a config reload
    /// takes effect on the next utterance without restarting capture.
    pub sensitivity: u8,
    /// How long capture may run before it is force-committed and the
    /// microphone closed.
    ///
    /// A safety net, not a feature: push-to-talk bounds itself on key
    /// release, but tap-to-latch waits for a second tap that may never
    /// come. docs/ux/02-core-interaction.md promised this and nothing
    /// implemented it.
    pub hot_mic_timeout_ms: u64,
    /// Keep the stream open this long after a commit, for devices measured
    /// slower than the pre-roll window. Zero disables it.
    ///
    /// A device that takes 210ms to deliver its first sample loses the
    /// head of every utterance, and no downstream buffer can recover audio
    /// the device never captured (docs/input-latency.md option 3 is
    /// explicit that widening pre-roll does NOT help). The only fix is to
    /// already be open when the user starts speaking.
    ///
    /// This trades the property that the recording indicator means
    /// "dictating right now", so it is deliberately narrow: opt-in, only
    /// for devices measured slow on this machine, and bounded so the
    /// indicator still goes out on its own.
    pub warm_hold_ms: u64,
    /// Resolve per-app settings for the app the user was looking at.
    ///
    /// A closure rather than a `config::Config` handle so the pipeline
    /// stays testable headlessly and so config reloads are picked up
    /// without restarting capture: the menu host owns the live config and
    /// this reads through it once per utterance.
    ///
    /// `None` means no profile support (the `--once` measurement path,
    /// and any host without a config), in which case the flat values on
    /// this struct are used unchanged.
    pub resolve_for_app: Option<AppResolver>,
    /// Live `microphone.sensitivity`, when the host has one to offer.
    ///
    /// `sensitivity` on this struct is a snapshot taken when `run` was
    /// called, so a config reload could never reach it: the segmenter is
    /// rebuilt at every key-down, but from the frozen copy, so changing the
    /// setting appeared to do nothing until restart. This closure reads the
    /// value the host holds now.
    ///
    /// `None` for hosts with no live config (the `--once` measurement path),
    /// which then use the flat `sensitivity` field unchanged.
    pub live_sensitivity: Option<std::sync::Arc<dyn Fn() -> u8 + Send + Sync>>,
    /// The merged vocabulary for `vocabulary.sets`, when any are active.
    ///
    /// `None` skips the correction pass entirely, which is the common path:
    /// most users configure no sets, and an empty pass over every transcript
    /// is work with no possible effect.
    pub vocabulary: Option<std::sync::Arc<config::vocab::Vocabulary>>,
}

/// Resolves per-app settings for the app the user was looking at.
///
/// Shared and thread-safe because the pipeline runs on the async event
/// loop while the menu host owns the config it reads through.
pub type AppResolver = Arc<dyn Fn(&config::AppIdentity) -> AppSettings + Send + Sync>;

/// The subset of settings a profile may change per utterance.
///
/// Deliberately small. Every field here has to be read at the moment it is
/// used rather than once at startup, so growing this set has a real cost;
/// a key belongs here only when "different in Slack than in Terminal" is
/// something a user actually wants.
#[derive(Debug, Clone, PartialEq)]
pub struct AppSettings {
    /// `enabled = false` in a profile mutes dictation for that app.
    pub enabled: bool,
    /// `insertion.mode = "stream"`.
    pub prefer_streaming: bool,
}

impl Default for Config {
    /// A plain interactive run. Exists so a test (or a future field) names
    /// only what is unusual about its case, rather than every field: the
    /// same reason `menubar::Settings` has one.
    fn default() -> Config {
        Config {
            once: false,
            auto_endpoint: false,
            prefer_streaming: false,
            // The schema default, kept in one place.
            sensitivity: 50,
            hot_mic_timeout_ms: 60_000,
            // Off unless the user asks: the default must be the honest
            // indicator, not the faster one.
            warm_hold_ms: 0,
            resolve_for_app: None,
            live_sensitivity: None,
            vocabulary: None,
        }
    }
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
    /// The app that held focus at key-down.
    ///
    /// Kept so the commit path can notice focus moving mid-utterance. The
    /// write lands a few hundred milliseconds after the key is released, and
    /// anything that raises a window in between silently redirects the text.
    /// Observed while testing: Discord raised itself and dictations aimed at
    /// Messages landed in Discord, which is indistinguishable from "it does
    /// not work in this app" unless someone says where the text went.
    targeted_app: Option<String>,
    released_at: Instant,
    /// Live streaming session, when the field accepted one. `None` is the
    /// buffered commit-on-release path, which is also every error path's
    /// fallback.
    streamer: Option<Streamer>,
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
    let mut segmenter = new_segmenter(current_sensitivity(&cfg));
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
    // Streaming writer completions ride their own channel; the sender is
    // cloned into each utterance's writer thread.
    let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel::<StreamerEvent>();
    // A streaming utterance whose final settle is still in the writer's
    // hands. The report is produced when `Finished` arrives.
    let mut pending_final: Option<PendingFinal> = None;
    // Per-device stream-startup latency watchdog (docs/input-latency.md).
    let mut startup = StartupWatch::new();
    // When the current capture opened, for the hot-mic safety net below.
    let mut capture_opened_at: Option<Instant> = None;
    // When a post-commit warm hold expires, if one is running.
    let mut warm_until: Option<Instant> = None;

    loop {
        // Computed each iteration: when the streamer's parked write becomes
        // due, so the select sleeps exactly until then instead of polling.
        let stream_deadline = in_flight
            .as_ref()
            .and_then(|f| f.streamer.as_ref())
            .and_then(|s| s.deadline());
        tokio::select! {
            // Biased: readiness resolves before any Final that raced past
            // it (the worker sends `ready` before touching audio, but the
            // random-order select could still poll the channels first).
            biased;

            // Hot-mic safety net. docs/ux/02-core-interaction.md promised
            // this and nothing implemented it, so a latched capture could
            // hold the microphone open indefinitely: `silence-timeout-ms`
            // was declared in the schema, marked unwired, and read by
            // nobody. An open microphone the user did not ask for is the
            // worst failure this product has, so the timeout is enforced
            // here regardless of how capture was started.
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(
                capture_opened_at
                    .map(|t| t + std::time::Duration::from_millis(cfg.hot_mic_timeout_ms))
                    .unwrap_or_else(Instant::now),
            )), if capture_opened_at.is_some() => {
                eprintln!(
                    "outloud: capture ran for {}s without ending; closing the microphone",
                    cfg.hot_mic_timeout_ms / 1000
                );
                // Commit rather than discard: the user spoke, and throwing
                // their words away to fix a stuck mic trades one silent
                // data loss for another.
                commit(&mut engine, &mut segmenter, &feed, &mut in_flight, &mut listening);
                if let Some(msg) = stop_capture(
                    mic.as_mut(),
                    &mut listening,
                    &mut startup,
                    &mut capture_opened_at,
                ) {
                    // The whole utterance was digital silence. Say so, with
                    // the likely cause: without this the key appears to do
                    // nothing at all, which reads as a broken app rather
                    // than a headset that belongs to a phone right now.
                    eprintln!("outloud: {msg}");
                    engine.transition(OverlayState::Error, Some(msg));
                }
            }

            // Warm hold expiring: close the device the user is no longer
            // dictating into. The hold is a latency optimisation, not a
            // licence to keep the stream, so it always ends by itself.
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(
                warm_until.unwrap_or_else(Instant::now),
            )), if warm_until.is_some() => {
                warm_until = None;
                if !listening {
                    // Warm-hold expiry: the user is not dictating, so this
                    // must not raise an overlay error out of nowhere. It is
                    // still logged, because a device that delivered nothing
                    // for a whole hold is worth a line in the diagnostics.
                    if let Some(msg) = stop_capture(
                        mic.as_mut(),
                        &mut listening,
                        &mut startup,
                        &mut capture_opened_at,
                    ) {
                        eprintln!("outloud: {msg}");
                    }
                }
            }

            // Flush a parked streamed write once its 80ms interval elapses.
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(
                stream_deadline.unwrap_or_else(Instant::now),
            )), if stream_deadline.is_some() => {
                if let Some(s) = in_flight.as_mut().and_then(|f| f.streamer.as_mut()) {
                    s.on_tick(Instant::now());
                }
            }

            // Writer-thread completions: unlock the coalescer, or settle
            // the utterance whose final pass just landed.
            Some(ev) = stream_rx.recv() => {
                match ev {
                    StreamerEvent::WriteDone(result) => {
                        if let Some(s) = in_flight.as_mut().and_then(|f| f.streamer.as_mut()) {
                            s.on_write_done(result, Instant::now());
                        }
                    }
                    StreamerEvent::Finished { result, wrote_any } => {
                        let Some(pf) = pending_final.take() else { continue };
                        let report = settle_streamed(
                            &mut engine, pf, result, wrote_any, &feed, recorder, cfg.vocabulary.as_deref(),
                        ).await;
                        if let Some(r) = report {
                            eprintln!("outloud: {}", r.render());
                            reports.push(r);
                        }
                        if cfg.once {
                            return Ok(reports);
                        }
                    }
                }
            }

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
                        eprintln!("outloud: recognizer ready: {name}");
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
                            eprintln!("outloud: key-down ignored, previous utterance still committing");
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
                        // Open the device FIRST. It does not depend on the
                        // snapshot, and the device is the slow part: a cold
                        // AX read costs up to 20.8ms and the stream needs
                        // ~64ms to deliver its first sample, so taking the
                        // snapshot first spent that AX time with the
                        // microphone shut. That is not latency the user
                        // waits out, it is audio the device never captured,
                        // and lost head audio is MISRECOGNISED rather than
                        // dropped ("quick" -> "Like"). See
                        // docs/investigations/latency.md.
                        //
                        // Opening HERE rather than at startup still holds:
                        // a stream open all session lights the recording
                        // indicator permanently, telling the user they are
                        // being recorded while idle. See mic.rs.
                        // A warm hold in flight means the stream is
                        // already up: adopt it rather than closing and
                        // reopening, which is the entire point.
                        let adopted_warm_stream = warm_until.take().is_some();
                        if let Some(m) = mic.as_mut() {
                            if let Err(e) = m.open() {
                                eprintln!("outloud: could not open the microphone: {e}");
                                engine.transition(
                                    OverlayState::Error,
                                    Some("could not open the microphone -> check Privacy settings".into()),
                                );
                                continue;
                            }
                            // Stamp the open so the first chunk's arrival
                            // measures this device's real startup latency.
                            //
                            // Skipped when adopting a warm stream: audio is
                            // already flowing, so the gap to the next chunk
                            // measures the poll interval rather than the
                            // device. Recording it would report ~0ms and
                            // teach the watchdog this device is fast, which
                            // would then withdraw the very hold that made
                            // it look fast.
                            if !adopted_warm_stream {
                                startup.on_open(Instant::now());
                            }
                            capture_opened_at = Some(Instant::now());
                        }
                        // Snapshot decides dictate-vs-edit from the selection
                        // the user is looking at. Still semantically at
                        // key-down: the microphone opening a few hundred
                        // microseconds earlier cannot change what is
                        // selected, and the stream spins up concurrently.
                        let span = recorder.start(Stage::Read);
                        let (mode, snap) = inject::snapshot_and_mode_at_keydown();
                        recorder.finish(span);
                        if let Mode::Edit { selected } = &mode {
                            eprintln!("outloud: edit mode on selection: \"{selected}\"");
                        }
                        // Per-app profile for the app the user was looking
                        // at when they pressed the key. Resolved here, not
                        // at commit: those differ exactly when a slow
                        // utterance races a window switch, and applying
                        // another app's rules to this app's text is the
                        // failure this ordering prevents.
                        let per_app = crate::inject::app_identity(snap.as_ref())
                            .and_then(|id| cfg.resolve_for_app.as_ref().map(|f| f(&id)));
                        if let Some(a) = &per_app {
                            if !a.enabled {
                                // A profile muted this app. Say so: silence
                                // that looks like a crash is what the
                                // `enabled` key is most likely to cause.
                                eprintln!(
                                    "outloud: dictation is disabled for this app by a profile"
                                );
                                engine.transition(OverlayState::Idle, None);
                                continue;
                            }
                        }
                        // Streaming, when asked for and the field can take
                        // in-place revisions. Everything else (edits, refused
                        // fields, other platforms) keeps commit-on-release.
                        let wants_stream = per_app
                            .as_ref()
                            .map_or(cfg.prefer_streaming, |a| a.prefer_streaming);
                        let streamer = if crate::streamer::wants_streaming(
                            wants_stream,
                            &mode,
                            snap.as_ref().and_then(|s| s.app.as_deref()),
                        ) {
                            snap.as_ref().and_then(|s| Streamer::begin(s, stream_tx.clone()))
                        } else {
                            None
                        };
                        segmenter = new_segmenter(current_sensitivity(&cfg));
                        in_flight = Some(InFlight {
                            mode,
                            // From the key-down snapshot, so the commit path
                            // can tell whether focus moved under it.
                            //
                            // Falls back to `frontmost_app()` only when the
                            // snapshot has no name at all, which happens
                            // whenever no text element is focused at key-down.
                            //
                            // The snapshot stays FIRST on purpose: it is
                            // captured atomically with the field, while
                            // `frontmost_app` races focus. Measured while
                            // checking this, the two genuinely disagree,
                            // frontmost_app said "Discord" when the snapshot
                            // said "Finder". So this is a last resort for
                            // "some name beats no name", not a preferred
                            // source.
                            //
                            // Correcting an earlier claim: I believed apps
                            // like Discord and Messages structurally hide
                            // their name here. They do not. Probed with their
                            // text fields actually focused, both report their
                            // own name. The Nones came from moments with no
                            // focused field, which is common at key-down but
                            // is not a property of the app.
                            targeted_app: snap
                                .as_ref()
                                .and_then(|s| s.app.clone())
                                .or_else(ax_edit::frontmost_app),
                            released_at: Instant::now(),
                            streamer,
                        });
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
                            // Drain the audio already captured before
                            // committing. Capture runs on the audio thread
                            // and arrives here through a channel, so when
                            // KeyUp is processed there are normally chunks
                            // queued behind it holding the last stretch of
                            // speech. Committing first sets listening=false
                            // and the Chunk arm then skips them, which drops
                            // the end of the sentence the user actually said.
                            //
                            // try_recv only takes what is already queued, so
                            // this cannot wait on audio the user has not
                            // spoken: it ends the moment the channel is empty.
                            //
                            // Non-chunk events are handled here the same way
                            // their arms below would: a bare `while let` on
                            // the Chunk pattern would silently DISCARD a
                            // queued KeyDown or capture event it popped.
                            while let Ok(queued) = frontend.try_recv() {
                                match queued {
                                    FrontendEvent::Chunk(samples) => {
                                        engine.live_audio(&samples);
                                        for ev in segmenter.push(&samples) {
                                            use audio::segment::SpeechEvent::*;
                                            match ev {
                                                SpeechStart { audio } | Partial { audio } => feed.push(audio),
                                                SpeechEnd { .. } => {}
                                            }
                                        }
                                    }
                                    // The commit below leaves in_flight set,
                                    // so the KeyDown arm would refuse this
                                    // press identically.
                                    FrontendEvent::KeyDown => eprintln!(
                                        "outloud: key-down ignored, previous utterance still committing"
                                    ),
                                    // Already committing; a duplicate end is
                                    // a no-op.
                                    FrontendEvent::KeyUp => {}
                                    FrontendEvent::CaptureUp(device) => {
                                        eprintln!("outloud: capturing from {device}");
                                        startup.on_device(&device);
                                    }
                                    FrontendEvent::CaptureIssue(msg) => {
                                        eprintln!("outloud: capture: {msg}");
                                        // F-6: this arm runs while the user is
                                        // still speaking, and a device change
                                        // here loses the rest of the sentence.
                                        // stderr alone is invisible to anyone
                                        // who launched the app from Finder, so
                                        // say it where they are already
                                        // looking. `live_detail` rather than a
                                        // transition because the utterance is
                                        // still in flight and Listening is the
                                        // honest state.
                                        engine.live_detail(format!(
                                            "microphone changed mid-sentence -> some audio may be lost ({msg})"
                                        ));
                                    }
                                }
                            }
                            commit(&mut engine, &mut segmenter, &feed, &mut in_flight, &mut listening);
                            // Normally: release the device as soon as the
                            // user stops speaking, so the recording
                            // indicator tracks dictation rather than
                            // uptime.
                            //
                            // Exception, opt-in and per-device: a device
                            // measured slower than the pre-roll window
                            // clips the head of every utterance, and the
                            // only cure is to already be open next time.
                            // Stop *listening* either way -- audio is
                            // discarded from here -- but defer the close.
                            if cfg.warm_hold_ms > 0 && startup.current_device_is_slow() {
                                listening = false;
                                capture_opened_at = None;
                                warm_until = Some(
                                    Instant::now()
                                        + std::time::Duration::from_millis(cfg.warm_hold_ms),
                                );
                            } else if let Some(msg) = stop_capture(
                                mic.as_mut(),
                                &mut listening,
                                &mut startup,
                                &mut capture_opened_at,
                            ) {
                                // The ordinary end of an utterance, and the
                                // one a user actually reaches after speaking
                                // into a mic that delivered nothing. Dropping
                                // the verdict here made the key look inert:
                                // no text, no error, no reason.
                                eprintln!("outloud: {msg}");
                                engine.transition(OverlayState::Error, Some(msg));
                            }
                        }
                    }
                    FrontendEvent::Chunk(samples) => {
                        if !listening {
                            continue; // mic is only *used* while capturing
                        }
                        // First chunk of the utterance: how late did this
                        // device actually start? A slow device silently
                        // corrupts the first word (docs/input-latency.md),
                        // so lateness is surfaced, once per device.
                        match startup.on_first_audio(Instant::now()) {
                            Verdict::Fine => {}
                            Verdict::SlowAgain { latency } => {
                                eprintln!(
                                    "outloud: slow capture start again ({}ms)",
                                    latency.as_millis()
                                );
                            }
                            Verdict::SlowFirstSample { message } => {
                                eprintln!("outloud: {message}");
                                // Detail on the listening overlay: the user
                                // is mid-utterance, and this is exactly the
                                // moment the advice applies.
                                engine.live_detail(message);
                            }
                            // Only `on_utterance_end` produces this, and it
                            // is matched where that is called. Listed rather
                            // than swept into a catch-all so that routing it
                            // to the wrong place stays a compile error.
                            Verdict::SilentCapture { .. } => {}
                        }
                        // Watch for a stream that opens, reports success and
                        // delivers nothing: a Bluetooth headset claimed by
                        // another device looks identical to a working one
                        // until you inspect the samples.
                        startup.on_audio(&samples);
                        engine.live_audio(&samples);
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
                        eprintln!("outloud: capturing from {device}");
                        startup.on_device(&device);
                    }
                    FrontendEvent::CaptureIssue(msg) => {
                        // Capture self-heals (rebuild loop); a total absence of
                        // input is the one unrecoverable case worth the Error
                        // state, with the next action named.
                        eprintln!("outloud: capture: {msg}");
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
                        // Streaming: stable prefixes go into the field via
                        // the horizon; the unstable tail is the overlay's
                        // ghost. Buffered: the whole hypothesis is ghost
                        // text and the field is touched once, at commit.
                        let shown = match in_flight.as_mut().and_then(|f| f.streamer.as_mut()) {
                            Some(s) => s.on_partial(&text, Instant::now()),
                            None => text,
                        };
                        engine.live(0.0, Some(&shown));
                    }
                    AsrEvent::Final(result) => {
                        let Some(fl) = in_flight.take() else {
                            eprintln!("outloud: stray final transcript ignored");
                            continue;
                        };
                        let finalize_ms = fl.released_at.elapsed().as_secs_f64() * 1000.0;
                        match result {
                            Ok(t) => {
                                let text = t.text.trim().to_string();
                                if let InFlight { streamer: Some(s), released_at, .. } = fl {
                                    // Streamed: the writer settles with one
                                    // consolidated correction; the report
                                    // lands when Finished arrives.
                                    if text.is_empty() {
                                        engine.transition(OverlayState::Idle, None);
                                        // The writer still needs its Finish
                                        // (it exits on it) but writes nothing.
                                        s.finish("", Instant::now());
                                        if cfg.once {
                                            return Ok(reports);
                                        }
                                        continue;
                                    }
                                    engine.transition(OverlayState::Injecting, None);
                                    let inject_started = Instant::now();
                                    s.finish(&text, inject_started);
                                    pending_final = Some(PendingFinal {
                                        transcript: text,
                                        finalize_ms,
                                        released_at,
                                        inject_started,
                                    });
                                    continue; // report comes with Finished
                                }
                                let report = commit_transcript(
                                    &mut engine, &fl, &text, finalize_ms, &feed, recorder, cfg.vocabulary.as_deref(),
                                ).await;
                                if let Some(r) = report {
                                    eprintln!("outloud: {}", r.render());
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
                            hold_for_inspection();
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
/// Stop capturing, by whatever route.
///
/// The microphone and the `listening` flag are two representations of one
/// fact, and every place that changed one of them by hand was a chance to
/// forget the other. One did: a sub-threshold tap latched capture on and
/// emitted no bounding event, so the mic stayed open and the recording
/// indicator stayed lit until the user pressed the chord again. Because a
/// bare modifier gets tapped constantly during ordinary typing, that fired
/// while *not* dictating, which is why it looked unreproducible.
///
/// Routing every stop through here makes the class impossible rather than
/// fixing the one path that was found.
/// `#[must_use]` because ignoring the return value is exactly the bug this
/// function existed to prevent: two of three callers silently dropped the
/// silence verdict, so a mic that delivered nothing produced no text and no
/// error. Discipline did not catch that; the compiler will.
#[must_use = "the silence verdict must be shown to the user, not dropped"]
fn stop_capture(
    mic: Option<&mut crate::mic::Mic>,
    listening: &mut bool,
    startup: &mut StartupWatch,
    opened_at: &mut Option<Instant>,
) -> Option<String> {
    *listening = false;
    *opened_at = None;
    if let Some(m) = mic {
        m.close();
    }
    startup.on_close();

    // Every route out of listening passes through here, which is why the
    // silence check lives in this function rather than at each of the three
    // commit sites: a fault that only some exits detect is a fault users
    // will hit through the exit nobody instrumented.
    //
    // Detecting it here is only half the job: the RETURNED message has to be
    // surfaced by each caller. Two of the three used to discard it, including
    // the ordinary key-release path, so the check ran on every exit and the
    // user still saw nothing. Callers must handle the Some.
    match startup.on_utterance_end() {
        Verdict::SilentCapture { message } => Some(message),
        _ => None,
    }
}

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

/// A streamed utterance whose final settle is executing on the writer
/// thread. Everything the report needs, minus the outcome.
struct PendingFinal {
    transcript: String,
    finalize_ms: f64,
    released_at: Instant,
    inject_started: Instant,
}

/// Produce the report (and states) for a streamed utterance once its final
/// settle lands. The fallback rule: a failed settle on an UNTOUCHED field
/// may still use the buffered insert path; once any streamed text is in the
/// field, a second full insert would duplicate words, so failure is
/// reported instead.
async fn settle_streamed(
    engine: &mut Engine,
    pf: PendingFinal,
    result: Result<(), String>,
    wrote_any: bool,
    feed: &AudioFeed,
    recorder: &mut Recorder,
    vocabulary: Option<&config::vocab::Vocabulary>,
) -> Option<UtteranceReport> {
    let inject_ms = pf.inject_started.elapsed().as_secs_f64() * 1000.0;
    recorder.record(
        Stage::Write,
        std::time::Duration::from_secs_f64(inject_ms / 1000.0),
    );
    let outcome = match result {
        Ok(()) => {
            engine.transition(OverlayState::Idle, None);
            "ax-stream".to_string()
        }
        Err(e) if !wrote_any => {
            // Nothing landed: the buffered path can still deliver whole.
            eprintln!("outloud: streamed settle failed on an untouched field ({e}); using the buffered path");
            let fl = InFlight {
                mode: Mode::Dictate,
                // The streamed attempt already established the target; this
                // retry writes wherever focus is now, and saying "unknown"
                // is honest rather than asserting a stale app name.
                targeted_app: None,
                released_at: pf.released_at,
                streamer: None,
            };
            return commit_transcript(
                engine,
                &fl,
                &pf.transcript,
                pf.finalize_ms,
                feed,
                recorder,
                vocabulary,
            )
            .await;
        }
        Err(e) => {
            let msg = format!(
                "streamed dictation could not finish cleanly ({e}) -> check the text and fix by hand"
            );
            engine.transition(OverlayState::Error, Some(msg.clone()));
            format!("ax-stream-failed: {msg}")
        }
    };
    Some(UtteranceReport {
        transcript: pf.transcript,
        outcome,
        finalize_ms: pf.finalize_ms,
        inject_ms,
        release_to_text_ms: pf.finalize_ms + inject_ms,
        dropped_chunks: feed.dropped_chunks(),
    })
}

/// The back half: transcript -> (parse -> apply ->) write -> state updates.
/// Returns `None` for the quiet empty-transcript path.
/// Keep the process alive after a `--once` run so its final overlay state can
/// be seen.
///
/// A visible state is only actually visible if something renders it, and a
/// run that exits on commit tears the overlay down before that can be
/// checked. Reading the log proves a value was computed; it does not prove a
/// user could ever read it. This closes that gap for anyone verifying by eye
/// or by screenshot.
fn hold_for_inspection() {
    let Some(ms) = std::env::var("OUTLOUD_HOLD_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    else {
        return;
    };
    eprintln!("outloud: holding {ms}ms so the final overlay state can be inspected");
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

async fn commit_transcript(
    engine: &mut Engine,
    fl: &InFlight,
    text: &str,
    finalize_ms: f64,
    feed: &AudioFeed,
    recorder: &mut Recorder,
    vocabulary: Option<&config::vocab::Vocabulary>,
) -> Option<UtteranceReport> {
    if text.is_empty() {
        // The documented "empty (silence) result" edge.
        engine.transition(OverlayState::Idle, None);
        return None;
    }
    engine.transition(OverlayState::Injecting, None);

    // Vocabulary correction, before any transport sees the text.
    //
    // Applied here rather than at each write site for the same reason the
    // transport decision was collapsed into one function: five separate
    // bypasses of one per-app rule shipped when the decision lived at the
    // call sites. This is the single funnel every finalised utterance passes
    // through, so a new transport cannot miss it.
    let corrected;
    let text = match vocabulary {
        Some(vocab) => {
            let (fixed, applied) = vocab.correct(text);
            if !applied.is_empty() {
                // Logged because a correction the user did not ask for is
                // indistinguishable from a recognizer error otherwise, and
                // "why did it write that" needs an answer.
                for c in &applied {
                    eprintln!("outloud: vocabulary: {:?} -> {:?}", c.from, c.to);
                }
            }
            corrected = fixed;
            corrected.as_str()
        }
        None => text,
    };

    // Did focus move while the user was speaking? The write goes wherever
    // focus is NOW, so if a window raised itself mid-utterance the text is
    // about to land somewhere the user was not looking. Reported rather than
    // prevented: they said the words, and refusing to type them would lose
    // the utterance entirely. Naming the destination is what turns "it does
    // not work in this app" into "it went to that one".
    // OUTLOUD_FAKE_TARGET: pretend the utterance was aimed at this app.
    //
    // Exists because the honest end-to-end test is otherwise a race that
    // cannot be won: raising a window takes longer than the time between
    // key-up and the write. Overriding the target makes the move certain
    // while still exercising the real focus lookup, message and overlay
    // path, which is the difference between "the unit test passes" and
    // "the user will see this".
    let targeted = std::env::var("OUTLOUD_FAKE_TARGET")
        .ok()
        .or_else(|| fl.targeted_app.clone());

    // OUTLOUD_PRECOMMIT_DELAY_MS: widen the key-up-to-write window so a real
    // focus move can be performed inside it.
    //
    // FAKE_TARGET fakes the key-down half, which means it cannot catch a bug
    // in that half. One did hide there: `targeted_app` came only from
    // `snapshot_focused()`, which fails outright when no text element is
    // focused, so the warning was dead in exactly the AX-hostile apps most
    // likely to steal focus. Slowing the race down instead exercises both
    // halves for real.
    if let Ok(ms) = std::env::var("OUTLOUD_PRECOMMIT_DELAY_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            eprintln!("outloud: delaying {ms}ms before the focus check (test knob)");
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
    }

    let moved_to = inject::focus_moved_to(targeted.as_deref());
    if let Some(landed_in) = &moved_to {
        eprintln!("outloud: focus moved while you spoke; this text is going to {landed_in}");
    }

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

    // The focus warning rides the terminal transition rather than being
    // published on its own. `live_detail` is explicitly cleared by the next
    // state change, and the next state change is one line below, so a detail
    // set before the write never survived to be read.
    // Where a successful utterance comes to rest. Normally Idle, but Error
    // when focus moved, because the text landed somewhere the user was not
    // looking, which is a failure from their side even though every
    // mechanical step reported success.
    //
    // Decided BEFORE transitioning, not corrected afterwards: Idle -> Error
    // is illegal by design (Idle is a resting state, not a route into an
    // error), so setting Idle first and fixing it up was silently dropped
    // and the warning never rendered. Error is reachable from Transcribing
    // and Injecting, which is where we actually are.
    //
    // Error rather than a detail on Idle because Idle renders nothing at all
    // (OverlayState::overlay_visible), so a note attached to it cannot be read.
    let settled = |engine: &mut Engine| match &moved_to {
        Some(app) => engine.transition(
            OverlayState::Error,
            Some(format!(
                "focus moved while you spoke -> your text went to {app}"
            )),
        ),
        None => engine.transition(OverlayState::Idle, None),
    };

    let outcome_str = match outcome {
        Outcome::Wrote { via, .. } => {
            settled(engine);
            via
        }
        Outcome::Suppressed { .. } => {
            settled(engine);
            "suppressed (OUTLOUD_NO_INJECT)".into()
        }
        Outcome::EmptyTranscript => {
            engine.transition(OverlayState::Idle, None);
            "empty".into()
        }
        Outcome::FreeformUnsupported { instruction } => {
            // Deliverable 3: freeform has no local LLM yet. Say so, name
            // what WAS heard and what to do instead. Never silent.
            //
            // TWO next actions, not one. This outcome now fires in two
            // situations that look identical from here: the user really
            // did ask for a rewrite (rephrase it as a command), or the
            // classifier in `inject::freeform` mistook their dictation
            // for an instruction and refused to overwrite the selection
            // (say it again behind "type:"). Naming only the first left
            // the second case a dead end, since nothing on screen told
            // the user the escape hatch existed. A false refusal is
            // supposed to cost one retry; without this line it cost a
            // mystery.
            let msg = format!(
                "freeform edit \"{instruction}\" needs the local LLM (not shipped yet) \
                 -> rephrase as change/replace/delete/add/case, or say \
                 \"type: {instruction}\" to dictate those words as text"
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
        Outcome::StagedShellIntent { command } => {
            // Not an error and not silence: nothing appeared on screen, so
            // without a cue the user cannot tell a staged intent from a
            // dropped utterance. Idle would show nothing; a transient
            // message says what to press next.
            let msg = format!("staged \"{command}\" -> press ^X^A on the command line");
            engine.transition(OverlayState::Idle, Some(msg.clone()));
            format!("staged-shell-intent: {msg}")
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
/// The sensitivity to build the segmenter with right now.
///
/// Prefers the host's live value so a config reload takes effect at the next
/// key-down rather than the next launch, and falls back to the startup
/// snapshot for hosts that have no live config.
fn current_sensitivity(cfg: &Config) -> u8 {
    let v = cfg
        .live_sensitivity
        .as_ref()
        .map(|f| f())
        .unwrap_or(cfg.sensitivity);
    // OUTLOUD_LOG_SENSITIVITY=1: report the value each utterance was actually
    // segmented with. The setting has no visible output of its own, so
    // without this the only way to check a reload arrived is to speak at the
    // threshold and judge by ear, which cannot distinguish "the reload did
    // not arrive" from "the new value was not what I expected".
    if std::env::var_os("OUTLOUD_LOG_SENSITIVITY").is_some_and(|x| x == "1") {
        eprintln!("outloud: segmenting with sensitivity {v}");
    }
    v
}

fn new_segmenter(sensitivity: u8) -> Segmenter {
    audio::segment::SpeechSegmenter::new(
        audio::vad::EnergyVad::from_sensitivity(sensitivity),
        audio::segment::SegmenterConfig::default(),
    )
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
    /// Injection is suppressed and must still report a *named* outcome, not
    /// panic: that is the graceful-degradation contract under test.
    #[tokio::test]
    async fn one_utterance_flows_end_to_end() {
        // Without this the test performs a REAL delivery. The old comment
        // claimed "no focused text field in CI", which is true in CI and
        // false on a developer's Mac: there the write fell through to the
        // clipboard transport, so running `cargo test` silently replaced
        // whatever the developer had copied with the fixture sentence.
        //
        // Caught by copying a sentinel string, running the suite, and
        // pasting. The suite was green throughout: the damage was outside
        // everything it asserted on.
        let _no_inject = crate::testenv::no_inject();

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
            prefer_streaming: false,
            ..Default::default()
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

    /// `prefer_streaming: true` with no streamable field must degrade to
    /// exactly the buffered behaviour: one report, one write attempt, no hang
    /// waiting for a writer thread that was never spawned. This is the
    /// degradation-matrix contract at the pipeline level, not just the
    /// session level.
    ///
    /// `OUTLOUD_NO_INJECT` is what makes "no streamable field" true rather
    /// than hoped for. The test previously relied on the environment having
    /// no accessibility focus, which holds on CI and does NOT hold on a
    /// developer's Mac: there the run took the live AX path, wrote into
    /// whatever window happened to be focused, and blocked on it. That is
    /// why this failed roughly one run in four under load while passing
    /// eight times out of eight on an idle machine, and why raising the
    /// timeout would have hidden the cause instead of fixing it.
    #[tokio::test]
    async fn streaming_preference_degrades_to_buffered_without_a_field() {
        // The variable is process-wide and tests run in parallel, so this
        // guard holds a lock shared with every other test that depends on
        // the switch. The local version scoped the variable correctly but
        // took no lock, which made concurrent deliver() tests fail.
        let _no_inject = crate::testenv::no_inject();

        let (ftx, frx) = tokio::sync::mpsc::unbounded_channel();
        let (atx, arx) = tokio::sync::mpsc::unbounded_channel();
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        let feed = recognize::spawn(|| Ok(Box::new(MockRecognizer::new()) as _), atx, rtx);
        let (engine, _shared) = Engine::new();
        let mut recorder = Recorder::new();

        ftx.send(FrontendEvent::KeyDown).unwrap();
        for chunk in voiced(3.0).chunks(1600) {
            ftx.send(FrontendEvent::Chunk(chunk.to_vec())).unwrap();
        }
        ftx.send(FrontendEvent::KeyUp).unwrap();

        let cfg = Config {
            once: true,
            auto_endpoint: false,
            prefer_streaming: true,
            ..Default::default()
        };
        let reports = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            run(
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
            ),
        )
        .await
        .expect("degraded streaming run must terminate, not wait on a writer")
        .unwrap();
        assert_eq!(reports.len(), 1);
        assert!(!reports[0].transcript.is_empty());
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
            prefer_streaming: false,
            ..Default::default()
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
            prefer_streaming: false,
            ..Default::default()
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

    /// Audio already captured when the key comes up must still be heard.
    ///
    /// Reported symptom: "it didn't get my full sentence when I let go".
    ///
    /// The mechanism is a race, not a recognizer problem. Capture runs on
    /// the audio thread and reaches this loop through a channel, so at the
    /// moment KeyUp is processed there are normally chunks already queued
    /// behind it holding the last ~100ms of speech. `commit` sets
    /// `listening = false` immediately, and the `Chunk` arm skips anything
    /// arriving after that, so the tail is discarded even though the user
    /// spoke it before releasing.
    ///
    /// The other tests all send every chunk *before* KeyUp, which is the
    /// one ordering the bug cannot occur in. This one interleaves the way
    /// the real channel does.
    #[tokio::test]
    async fn audio_queued_behind_keyup_still_reaches_the_recognizer() {
        let (ftx, frx) = tokio::sync::mpsc::unbounded_channel();
        let (atx, arx) = tokio::sync::mpsc::unbounded_channel();
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        let feed = recognize::spawn(|| Ok(Box::new(MockRecognizer::new()) as _), atx, rtx);
        let (engine, _shared) = Engine::new();
        let mut recorder = Recorder::new();

        let audio = voiced(3.0);
        let chunks: Vec<Vec<f32>> = audio.chunks(1600).map(|c| c.to_vec()).collect();
        let split = chunks.len() / 2;

        ftx.send(FrontendEvent::KeyDown).unwrap();
        for chunk in &chunks[..split] {
            ftx.send(FrontendEvent::Chunk(chunk.clone())).unwrap();
        }
        ftx.send(FrontendEvent::KeyUp).unwrap();
        // Spoken before the release, delivered after it. This is the tail
        // the user loses.
        for chunk in &chunks[split..] {
            ftx.send(FrontendEvent::Chunk(chunk.clone())).unwrap();
        }

        let cfg = Config {
            once: true,
            auto_endpoint: false,
            prefer_streaming: false,
            ..Default::default()
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

        // MockRecognizer's transcript length tracks how much voiced audio it
        // was fed, so a truncated tail shows up as a shorter transcript.
        // Compare against the same audio delivered entirely before KeyUp.
        let baseline = transcript_for_all_audio_before_keyup(&chunks).await;
        let got = &reports[0].transcript;
        assert_eq!(
            got.split_whitespace().count(),
            baseline.split_whitespace().count(),
            "audio queued behind KeyUp was dropped: got {got:?}, but the same \
             audio delivered before KeyUp yields {baseline:?}"
        );
    }

    /// The control for the test above: identical audio, all of it delivered
    /// before the key is released.
    async fn transcript_for_all_audio_before_keyup(chunks: &[Vec<f32>]) -> String {
        let (ftx, frx) = tokio::sync::mpsc::unbounded_channel();
        let (atx, arx) = tokio::sync::mpsc::unbounded_channel();
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        let feed = recognize::spawn(|| Ok(Box::new(MockRecognizer::new()) as _), atx, rtx);
        let (engine, _shared) = Engine::new();
        let mut recorder = Recorder::new();

        ftx.send(FrontendEvent::KeyDown).unwrap();
        for chunk in chunks {
            ftx.send(FrontendEvent::Chunk(chunk.clone())).unwrap();
        }
        ftx.send(FrontendEvent::KeyUp).unwrap();

        let cfg = Config {
            once: true,
            auto_endpoint: false,
            prefer_streaming: false,
            ..Default::default()
        };
        run(
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
        .unwrap()[0]
            .transcript
            .clone()
    }

    /// The pipeline must read sensitivity through the live view when it has
    /// one, and fall back to the startup snapshot when it does not.
    ///
    /// The unit test on RuntimeShared proves the value can be published. This
    /// proves the pipeline actually READS it, which is the half that was
    /// broken: the setting was published to a runtime nobody consulted.
    #[test]
    fn live_sensitivity_overrides_the_startup_snapshot() {
        let snapshot_only = Config {
            sensitivity: 50,
            live_sensitivity: None,
            ..Default::default()
        };
        assert_eq!(
            current_sensitivity(&snapshot_only),
            50,
            "with no live view, the startup value stands"
        );

        let live = Config {
            sensitivity: 50,
            live_sensitivity: Some(std::sync::Arc::new(|| 80)),
            ..Default::default()
        };
        assert_eq!(
            current_sensitivity(&live),
            80,
            "a reload must beat the value copied at startup"
        );
    }
}
