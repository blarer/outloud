//! Microphone capture via cpal, with device enumeration and hotplug
//! recovery.
//!
//! This is the only module in the crate that touches hardware, kept thin on
//! purpose: it converts whatever the device offers into 16kHz mono f32 and
//! pushes it into the ring buffer. All decisions (VAD, segmentation) happen
//! downstream where they are testable.
//!
//! Hotplug (R-01: AirPods connecting mid-utterance) is handled by detection
//! and rebuild rather than by trusting the OS stream to survive: cpal
//! surfaces device death through the stream error callback, and we also poll
//! the default-device name because macOS *silently reroutes* the default
//! input on AirPods connect without erroring the old stream. On either
//! signal the capturer tears down and rebuilds on the new default. The ring
//! buffer keeps its contents across the swap, so at most the rebuild gap
//! (tens of ms) is lost, well inside R-01's "lose < 1s" acceptance bar.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::resample::{downmix, Resampler};
use crate::ring::Producer;
use crate::SAMPLE_RATE;

/// A capture-capable device, as shown to the user in settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    pub name: String,
    pub is_default: bool,
}

/// List input devices. Errors from individual devices are skipped rather
/// than failing the whole enumeration, because macOS routinely reports
/// half-initialized aggregate devices during hotplug transitions.
pub fn input_devices() -> anyhow::Result<Vec<InputDevice>> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(device_name);
    let mut out = Vec::new();
    for device in host.input_devices()? {
        if let Some(name) = device_name(device) {
            out.push(InputDevice {
                is_default: Some(&name) == default_name.as_ref(),
                name,
            });
        }
    }
    Ok(out)
}

/// Human-readable device name, or `None` when the device is mid-hotplug and
/// refuses to describe itself (macOS aggregate devices do this).
fn device_name(device: cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_string())
}

/// Does a device name satisfy the user's configured `microphone` value?
///
/// Exact match (case-insensitive) OR substring: device names are long and
/// vendor-decorated ("Headset Microphone (Razer BlackShark V2 Pro)"), and
/// requiring the user to transcribe one exactly is a setting that works on
/// the second try instead of the first. Pure so it is testable without
/// hardware.
pub fn matches_preference(device_name: &str, preference: &str) -> bool {
    let d = device_name.to_lowercase();
    let p = preference.trim().to_lowercase();
    !p.is_empty() && (d == p || d.contains(&p))
}

/// Capture-loop lifecycle events, delivered on the supervisor thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEvent {
    /// A stream is up and feeding the ring buffer.
    Started { device: String },
    /// The device died or the default changed; a rebuild is in progress.
    DeviceChanged { from: String },
    /// Rebuild failed; will retry until stopped.
    Error { message: String },
}

/// Handle that keeps capture alive. Dropping it (or calling [`stop`]) ends
/// the supervisor thread and closes the stream.
///
/// [`stop`]: CaptureHandle::stop
pub struct CaptureHandle {
    running: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CaptureHandle {
    /// The flag that goes false when this capture stops.
    ///
    /// Exposed so a consumer draining the ring can die with the stream that
    /// fills it. Without this, per-utterance capture leaks one drain task
    /// per dictation, each polling a ring nothing writes to.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running)
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Start capturing from the preferred (or default) input device into
/// `producer`, normalizing to 16kHz mono f32. Lifecycle events go to
/// `on_event`.
///
/// `preferred` is the `microphone` config value: a device name matched by
/// [`matches_preference`]. `None` (or no matching device) follows the
/// system default, and a configured-but-missing device is reported through
/// an `Error` event once per rebuild rather than silently ignored: the
/// user asked for a specific microphone, and recording from a different
/// one without saying so presents as "it hears nothing" (the exact failure
/// the audio-input diagnostic's module doc describes costing an hour).
///
/// The supervisor thread owns the cpal stream (cpal streams are not `Send`
/// on all platforms, so they must live on the thread that made them) and
/// polls for device changes every 500ms: cheap, and fast enough that a
/// mid-utterance AirPods switch loses far less than the 1s R-01 allows.
pub fn start_capture_preferring(
    producer: Producer,
    preferred: Option<String>,
    on_event: impl Fn(CaptureEvent) + Send + 'static,
) -> CaptureHandle {
    let running = Arc::new(AtomicBool::new(true));
    let running_thread = Arc::clone(&running);

    let thread = std::thread::Builder::new()
        .name("audio-capture".into())
        .spawn(move || supervisor_loop(producer, preferred, on_event, running_thread))
        .expect("spawning capture thread");

    CaptureHandle {
        running,
        thread: Some(thread),
    }
}

/// [`start_capture_preferring`] with no preference: follow the system
/// default. The signature the examples and device-sharing tests use.
pub fn start_capture(
    producer: Producer,
    on_event: impl Fn(CaptureEvent) + Send + 'static,
) -> CaptureHandle {
    start_capture_preferring(producer, None, on_event)
}

fn supervisor_loop(
    producer: Producer,
    preferred: Option<String>,
    on_event: impl Fn(CaptureEvent) + Send + 'static,
    running: Arc<AtomicBool>,
) {
    let host = cpal::default_host();
    while running.load(Ordering::SeqCst) {
        // Resolve the preference fresh on every (re)build, so plugging the
        // named headset in mid-session wins on the next rebuild without a
        // restart.
        let mut preferred_device = None;
        if let Some(want) = preferred.as_deref() {
            if let Ok(devices) = host.input_devices() {
                for d in devices {
                    let matches = d
                        .description()
                        .ok()
                        .is_some_and(|desc| matches_preference(desc.name(), want));
                    if matches {
                        preferred_device = Some(d);
                        break;
                    }
                }
            }
            if preferred_device.is_none() {
                // Once per rebuild attempt, not once per 500ms poll: this
                // event becomes a menu row, and a row that reposts itself
                // twice a second is noise nobody reads.
                on_event(CaptureEvent::Error {
                    message: format!(
                        "configured microphone \"{want}\" not found; using the system default"
                    ),
                });
            }
        }
        // Following the default (no preference, or preference missing)?
        // Then a default-device change triggers a rebuild below. A pinned
        // device must NOT rebuild on default changes: ignoring the default
        // is the entire point of pinning.
        let following_default = preferred_device.is_none();

        let Some(device) = preferred_device.or_else(|| host.default_input_device()) else {
            on_event(CaptureEvent::Error {
                message: "no input device available".into(),
            });
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        let active_name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "<unknown>".into());
        // The stream error callback fires on a cpal-internal thread; it only
        // flips this flag so the supervisor rebuilds from its own thread.
        let died = Arc::new(AtomicBool::new(false));

        let stream = match build_stream(&device, &producer, Arc::clone(&died)) {
            Ok(s) => s,
            Err(e) => {
                on_event(CaptureEvent::Error {
                    message: format!("opening {active_name}: {e}"),
                });
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        if let Err(e) = stream.play() {
            on_event(CaptureEvent::Error {
                message: format!("starting {active_name}: {e}"),
            });
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        on_event(CaptureEvent::Started {
            device: active_name.clone(),
        });

        // Watch for death or (only when following it) default change.
        while running.load(Ordering::SeqCst) && !died.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(500));
            if following_default {
                let current_default = host.default_input_device().and_then(device_name);
                if current_default.as_deref() != Some(active_name.as_str()) {
                    break; // default moved (e.g. AirPods connected): rebuild
                }
            }
        }
        drop(stream);
        if running.load(Ordering::SeqCst) {
            on_event(CaptureEvent::DeviceChanged {
                from: active_name.clone(),
            });
        }
    }
}

/// Build an input stream on `device` that lands 16kHz mono f32 in the ring.
fn build_stream(
    device: &cpal::Device,
    producer: &Producer,
    died: Arc<AtomicBool>,
) -> anyhow::Result<cpal::Stream> {
    let config = device.default_input_config()?;
    let channels = config.channels() as usize;
    let in_rate_hz: u32 = config.sample_rate();

    // Resampler state must live in the callback; wrap in a Mutex because the
    // closure is Fn-called from one audio thread only (no contention).
    let resampler = Mutex::new(Resampler::new(in_rate_hz, SAMPLE_RATE));
    let producer = clone_producer(producer);

    let err_fn = move |_e: cpal::Error| {
        died.store(true, Ordering::SeqCst);
    };

    let stream = device.build_input_stream(
        config.into(),
        move |data: &[f32], _info: &cpal::InputCallbackInfo| {
            let mono = downmix(data, channels);
            let resampled = resampler
                .lock()
                .expect("resampler lock poisoned")
                .process(&mono);
            producer.push(&resampled);
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

/// The ring's Producer is intentionally not Clone in its public API (SPSC),
/// but the supervisor rebuilds streams over time, one at a time, so serial
/// reuse is safe. This helper documents that decision instead of hiding it.
fn clone_producer(p: &Producer) -> Producer {
    p.serial_clone()
}

#[cfg(test)]
mod tests {
    use super::matches_preference;

    /// The matching rule the `microphone` config key promises: exact or
    /// substring, case-insensitive, never empty.
    #[test]
    fn preference_matching_is_forgiving_but_not_empty() {
        let device = "Headset Microphone (Razer BlackShark V2 Pro)";
        assert!(matches_preference(device, device));
        assert!(matches_preference(device, "blackshark"));
        assert!(matches_preference(device, "BLACKSHARK V2"));
        assert!(matches_preference(device, "  blackshark  "));
        assert!(!matches_preference(device, "airpods"));
        // Empty or whitespace must not match everything: an empty string is
        // a config mistake, and matching device zero silently would pin the
        // user to an arbitrary microphone.
        assert!(!matches_preference(device, ""));
        assert!(!matches_preference(device, "   "));
    }
}
