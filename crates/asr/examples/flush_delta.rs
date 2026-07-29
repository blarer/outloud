//! Does the OS's end-of-input flush actually CHANGE the text?
//!
//! `finalize` costs ~20ms per second of audio (measured in helper_split),
//! and all of it is the analyzer re-running after stdin closes. If the last
//! volatile partial already equals the final transcript, that cost buys
//! nothing on this utterance and an early-commit strategy is on the table.
//! If it revises, the cost is accuracy and must be paid.
//!
//! Prints the last partial seen BEFORE stdin closed next to the final, plus
//! a verdict, for a corpus of sentences.
//!
//! Usage: cargo run --release -p asr --example flush_delta -- [realtime|fast]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

fn synth(text: &str) -> Vec<f32> {
    let dir = std::env::temp_dir().join(format!("asr-delta-{}", std::process::id()));
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
    std::env::var_os("AQUA_SPEECH_HELPER")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("helper")
                .join("aqua-speech-helper")
        })
}

fn text_of(line: &str) -> String {
    line.split("\"text\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap_or("")
        .to_string()
}

fn kind_of(line: &str) -> String {
    line.split("\"type\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap_or("?")
        .to_string()
}

fn run_one(sentence: &str, realtime: bool) {
    let samples = synth(sentence);
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
            if tx.send((kind_of(&line), text_of(&line))).is_err() {
                break;
            }
        }
    });
    let _ = rx.recv().unwrap(); // ready

    for chunk in samples.chunks(480) {
        let mut bytes = Vec::with_capacity(chunk.len() * 4);
        for s in chunk {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
            break;
        }
        if realtime {
            std::thread::sleep(std::time::Duration::from_secs_f64(480.0 / 16_000.0));
        }
    }

    // Everything the helper had already emitted when the "key came up".
    let mut before = String::new();
    while let Ok((k, t)) = rx.try_recv() {
        if k == "partial" || k == "final" {
            before = t;
        }
    }

    let t_close = Instant::now();
    drop(stdin);
    let mut after = before.clone();
    loop {
        let (k, t) = rx.recv().unwrap();
        match k.as_str() {
            "partial" | "final" => after = t,
            "done" => break,
            _ => {}
        }
    }
    let flush_ms = t_close.elapsed().as_secs_f64() * 1e3;
    let _ = child.wait();

    let verdict = if before == after {
        "IDENTICAL"
    } else if !before.is_empty() && after.starts_with(before.trim_end_matches(['.', ' '])) {
        "EXTENDED"
    } else {
        "REVISED"
    };
    println!(
        "{:.1}s flush {flush_ms:>4.0}ms  {verdict:<9} before={:?}\n{:>28}after={:?}",
        samples.len() as f32 / 16_000.0,
        before,
        "",
        after
    );
}

fn main() {
    let realtime = std::env::args()
        .nth(1)
        .map(|s| s == "realtime")
        .unwrap_or(false);
    println!("pacing = {}", if realtime { "realtime" } else { "fast" });
    for s in [
        "Hello there friend.",
        "The quick brown fox jumps over the lazy dog.",
        "Please send the report to Sarah before the meeting on Thursday.",
        "I think we should refactor the pipeline module because it has grown too large and hard to follow.",
        "Let me know what you think about the new design once you have had a chance to look it over carefully.",
    ] {
        run_one(s, realtime);
    }
}
