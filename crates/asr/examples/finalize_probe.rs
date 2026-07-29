//! Stage-level probe for the Apple recognizer's finalize cost.
//!
//! Answers: of the "release -> final transcript" number the pipeline
//! reports, how much is (a) audio the helper had not yet consumed when
//! stdin closed, (b) the OS speech stack's own end-of-input flush, and
//! (c) helper process teardown (`child.wait()`).
//!
//! Two pacings, mirroring the daemon's two realities:
//!   - `realtime`: audio fed at wall-clock speed, so the analyzer is caught
//!     up at release (a human speaking).
//!   - `fast`: all audio dumped as fast as the pipe takes it, so a backlog
//!     exists at release (a --wav / --say replay, and a fast talker).
//!
//! Usage: cargo run --release -p asr --example finalize_probe -- [secs] [pacing]

use std::time::Instant;

use asr::backends::apple::AppleRecognizer;
use asr::Recognizer;

fn synth(text: &str) -> Vec<f32> {
    let dir = std::env::temp_dir().join(format!("asr-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let aiff = dir.join("p.aiff");
    let caf = dir.join("p.caf");
    let st = |c: &str, a: &[&str]| {
        assert!(
            std::process::Command::new(c)
                .args(a)
                .status()
                .unwrap()
                .success(),
            "{c} failed"
        );
    };
    st("say", &["-o", aiff.to_str().unwrap(), text]);
    st(
        "afconvert",
        &[
            "-f",
            "caff",
            "-d",
            "LEF32@16000",
            "-c",
            "1",
            aiff.to_str().unwrap(),
            caf.to_str().unwrap(),
        ],
    );
    let bytes = std::fs::read(&caf).unwrap();
    let idx = bytes.windows(4).position(|w| w == b"data").unwrap();
    bytes[idx + 16..]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let text = args
        .first()
        .cloned()
        .unwrap_or_else(|| "The quick brown fox jumps over the lazy dog.".into());
    let realtime = args.get(1).map(|s| s == "realtime").unwrap_or(false);
    let reps: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);

    let samples = synth(&text);
    let secs = samples.len() as f32 / 16_000.0;
    println!(
        "audio: {:.2}s ({} samples), pacing={}",
        secs,
        samples.len(),
        if realtime { "realtime" } else { "fast" }
    );

    for rep in 0..reps {
        let t_spawn = Instant::now();
        let mut rec = AppleRecognizer::new().expect("helper");
        let spawn_ms = t_spawn.elapsed().as_secs_f64() * 1e3;

        // The daemon feeds 30ms segmenter frames (480 samples).
        let frame = 480;
        let t_feed = Instant::now();
        let mut feed_total_ms = 0.0f64;
        let mut feed_max_ms: f64 = 0.0;
        let mut partials = 0usize;
        let mut first_partial_ms: Option<f64> = None;
        for chunk in samples.chunks(frame) {
            let t = Instant::now();
            let p = rec.feed(chunk);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            feed_total_ms += ms;
            feed_max_ms = feed_max_ms.max(ms);
            if p.is_some() {
                partials += 1;
                first_partial_ms.get_or_insert(t_feed.elapsed().as_secs_f64() * 1e3);
            }
            if realtime {
                std::thread::sleep(std::time::Duration::from_secs_f64(frame as f64 / 16_000.0));
            }
        }

        // THE number: stdin close -> transcript in hand. This is exactly
        // what the pipeline's `finalize_ms` measures (minus channel hops).
        let t_fin = Instant::now();
        let t = rec.finalize().expect("finalize");
        let finalize_ms = t_fin.elapsed().as_secs_f64() * 1e3;

        println!(
            "rep{rep}: spawn {spawn_ms:.0}ms | feed sum {feed_total_ms:.1}ms max {feed_max_ms:.1}ms \
             | partials {partials} first@{:.0}ms | FINALIZE {finalize_ms:.0}ms | \"{}\"",
            first_partial_ms.unwrap_or(-1.0),
            t.text
        );
    }
}
