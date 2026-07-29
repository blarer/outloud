//! Splits the Apple backend's `finalize` into its two halves, because the
//! pipeline's `finalize_ms` is a single opaque number and the fix differs
//! entirely depending on which half dominates:
//!
//!   A. stdin-close -> helper prints `done`  (the OS speech stack flushing)
//!   B. `done` -> `child.wait()` returns     (process teardown, pure waste
//!      on the user's critical path: the transcript is already in hand)
//!
//! Drives the helper directly over pipes rather than through
//! `AppleRecognizer`, so each event can be timestamped as it arrives.
//!
//! Usage: cargo run --release -p asr --example helper_split -- "text" [realtime|fast] [reps]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

fn synth(text: &str) -> Vec<f32> {
    let dir = std::env::temp_dir().join(format!("asr-split-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let aiff = dir.join("p.aiff");
    let caf = dir.join("p.caf");
    let st = |c: &str, a: &[&str]| {
        assert!(
            Command::new(c).args(a).status().unwrap().success(),
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

fn helper_path() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("AQUA_SPEECH_HELPER") {
        return p.into();
    }
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("helper")
        .join("aqua-speech-helper")
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
    println!(
        "audio: {:.2}s, pacing={}",
        samples.len() as f32 / 16_000.0,
        if realtime { "realtime" } else { "fast" }
    );

    for rep in 0..reps {
        let t_spawn = Instant::now();
        let mut child = Command::new(helper_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn helper");
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<(String, Instant)>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let kind = line
                    .split("\"type\":\"")
                    .nth(1)
                    .and_then(|s| s.split('"').next())
                    .unwrap_or("?")
                    .to_string();
                if tx.send((kind, Instant::now())).is_err() {
                    break;
                }
            }
        });
        // ready
        let (k, _) = rx.recv().unwrap();
        assert_eq!(k, "ready");
        let ready_ms = t_spawn.elapsed().as_secs_f64() * 1e3;

        for chunk in samples.chunks(480) {
            let mut bytes = Vec::with_capacity(chunk.len() * 4);
            for s in chunk {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            stdin.write_all(&bytes).unwrap();
            stdin.flush().unwrap();
            if realtime {
                std::thread::sleep(std::time::Duration::from_secs_f64(480.0 / 16_000.0));
            }
        }

        // === the measured window ===
        let t_close = Instant::now();
        drop(stdin);
        let mut last_result_ms = f64::NAN;
        let done_ms;
        loop {
            let (kind, at) = rx.recv().unwrap();
            let ms = at.duration_since(t_close).as_secs_f64() * 1e3;
            match kind.as_str() {
                "partial" | "final" => last_result_ms = ms,
                "done" => {
                    done_ms = ms;
                    break;
                }
                other => eprintln!("  unexpected {other}"),
            }
        }
        let t_wait = Instant::now();
        let _ = child.wait();
        let wait_ms = t_wait.elapsed().as_secs_f64() * 1e3;

        println!(
            "rep{rep}: ready {ready_ms:.0}ms || close->last-text {last_result_ms:.0}ms  \
             close->done {done_ms:.0}ms  done->exit(child.wait) {wait_ms:.0}ms  \
             TOTAL finalize {:.0}ms",
            done_ms + wait_ms
        );
    }
}
