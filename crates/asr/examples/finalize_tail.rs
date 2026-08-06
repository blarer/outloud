//! Is `finalize` cost driven by the whole utterance, or only by the tail
//! the analyzer had not yet transcribed when stdin closed?
//!
//! This decides whether the biggest single latency lever is real. Measured
//! in flush_delta: with `.fastResults` the last volatile partial trails the
//! spoken text by ~1.5-2s, so finalize always has a tail to do. But
//! finalize also *scales with total audio length* (60ms @ 2.5s, 236ms @
//! 12.4s), which the tail theory alone does not explain.
//!
//! Method: feed the same audio, then wait `settle` seconds (with silence
//! fed, so the analyzer keeps running) before closing stdin. If finalize
//! collapses to a constant, the cost is the TAIL and a trailing-silence /
//! early-close strategy can hide it. If it stays proportional to length,
//! the analyzer re-runs the whole utterance at end-of-input and only a
//! different backend or a shorter utterance helps.
//!
//! Usage: cargo run --release -p asr --example finalize_tail

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

fn synth(text: &str) -> Vec<f32> {
    let dir = std::env::temp_dir().join(format!("asr-tail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let aiff = dir.join("p.aiff");
    let caf = dir.join("p.caf");
    let st = |c: &str, a: &[&str]| {
        assert!(Command::new(c).args(a).status().unwrap().success(), "{c}");
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

fn helper_path() -> std::path::PathBuf {
    std::env::var_os("OUTLOUD_SPEECH_HELPER")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("helper")
                .join("outloud-speech-helper")
        })
}

/// Feed `samples` at real-time pace, then `settle_secs` of silence, then
/// close stdin and time until `done`.
fn measure(samples: &[f32], settle_secs: f64) -> (f64, String) {
    let mut child = Command::new(helper_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<(String, String)>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let kind = line
                .split("\"type\":\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or("?")
                .to_string();
            let text = line
                .split("\"text\":\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or("")
                .to_string();
            if tx.send((kind, text)).is_err() {
                break;
            }
        }
    });
    let _ = rx.recv().unwrap(); // ready

    let feed = |stdin: &mut std::process::ChildStdin, chunk: &[f32]| {
        let mut bytes = Vec::with_capacity(chunk.len() * 4);
        for s in chunk {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let _ = stdin.write_all(&bytes).and_then(|_| stdin.flush());
    };
    for chunk in samples.chunks(480) {
        feed(&mut stdin, chunk);
        std::thread::sleep(std::time::Duration::from_secs_f64(480.0 / 16_000.0));
    }
    // Trailing silence at real-time pace: the analyzer keeps consuming, so
    // it can catch up on the tail while the user is already done speaking.
    let silence = vec![0f32; 480];
    let frames = (settle_secs * 16_000.0 / 480.0) as usize;
    for _ in 0..frames {
        feed(&mut stdin, &silence);
        std::thread::sleep(std::time::Duration::from_secs_f64(480.0 / 16_000.0));
    }

    let t = Instant::now();
    drop(stdin);
    let mut text = String::new();
    loop {
        let (k, s) = rx.recv().unwrap();
        match k.as_str() {
            "partial" | "final" => text = s,
            "done" => break,
            _ => {}
        }
    }
    let ms = t.elapsed().as_secs_f64() * 1e3;
    let _ = child.wait();
    (ms, text)
}

fn main() {
    let short = synth("The quick brown fox jumps over the lazy dog.");
    let long = synth("One two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty twenty one twenty two twenty three twenty four twenty five twenty six twenty seven twenty eight twenty nine thirty thirty one thirty two.");
    for (name, samples) in [("short", &short), ("long", &long)] {
        println!("--- {name}: {:.1}s audio", samples.len() as f32 / 16_000.0);
        for settle in [0.0, 0.5, 1.0, 2.0] {
            let (ms, text) = measure(samples, settle);
            println!(
                "  settle {settle:.1}s -> finalize {ms:>4.0}ms  words={}",
                text.split_whitespace().count()
            );
        }
    }
}
