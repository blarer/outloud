// Transcribe a real recording with the real model.
//
// The unit tests cover buffering arithmetic and error messages, which is
// everything reachable without the native library. None of it proves the
// backend transcribes anything, and "the tests pass while the feature is
// dead" is a mistake this project has made repeatedly today.
//
// Run with:
//   cargo run -p asr --features whisper --example transcribe -- MODEL WAV
use std::io::Read;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let model = args
        .next()
        .expect("usage: transcribe <model.bin> <file.wav>");
    let wav = args
        .next()
        .expect("usage: transcribe <model.bin> <file.wav>");

    // Minimal 16-bit PCM WAV reader: enough for the project's own fixtures,
    // and avoids a dependency for a debugging example.
    let mut bytes = Vec::new();
    std::fs::File::open(&wav)?.read_to_end(&mut bytes)?;
    let data_at = bytes
        .windows(4)
        .position(|w| w == b"data")
        .expect("no data chunk")
        + 8;
    let samples: Vec<f32> = bytes[data_at..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect();

    println!(
        "audio: {} samples, {:.2}s",
        samples.len(),
        samples.len() as f32 / 16_000.0
    );

    let started = std::time::Instant::now();
    let mut rec =
        asr::backends::whisper_cpp::WhisperCppRecognizer::new(std::path::Path::new(&model))?;
    println!("model loaded in {:?}", started.elapsed());

    use asr::Recognizer;
    let started = std::time::Instant::now();
    rec.feed(&samples);
    let out = rec.finalize()?;
    let elapsed = started.elapsed();

    println!("transcribed in {elapsed:?}");
    println!(
        "realtime factor: {:.1}x",
        out.audio_secs / elapsed.as_secs_f32()
    );
    println!("text: {:?}", out.text);
    Ok(())
}
