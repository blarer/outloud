//! What the audio hot path costs per capture callback, and how many
//! allocations it makes.
//!
//! The chain for one cpal callback is: downmix -> resample -> ring push,
//! then (on the drain tick) a Vec copy out of the ring, a channel send, a
//! segmenter push, and a per-frame `to_vec()` into the recognizer feed.
//! Each arrow is a fresh `Vec`. This measures whether that matters against
//! the ~30ms of wall time each frame represents.
//!
//! Run: cargo run --release -p audio --example hotpath_cost

use std::time::Instant;

fn main() {
    // A realistic callback: 48kHz stereo, 512 frames (the common CoreAudio
    // buffer size), which is ~10.7ms of audio.
    let channels = 2usize;
    let frames = 512usize;
    let input: Vec<f32> = (0..frames * channels)
        .map(|i| (i as f32 * 0.01).sin() * 0.2)
        .collect();

    let reps = 20_000;

    // 1. downmix
    let t = Instant::now();
    let mut sink = 0.0f32;
    for _ in 0..reps {
        let mono = audio::resample::downmix(&input, channels);
        sink += mono[0];
    }
    let downmix_us = t.elapsed().as_secs_f64() * 1e6 / reps as f64;

    // 2. resample 48k -> 16k
    let mono = audio::resample::downmix(&input, channels);
    let mut rs = audio::resample::Resampler::new(48_000, 16_000);
    let t = Instant::now();
    for _ in 0..reps {
        let out = rs.process(&mono);
        sink += out[0];
    }
    let resample_us = t.elapsed().as_secs_f64() * 1e6 / reps as f64;

    // 3. ring push
    let (producer, consumer) = audio::ring::ring(16_000 * 10);
    let resampled = rs.process(&mono);
    let t = Instant::now();
    for _ in 0..reps {
        producer.push(&resampled);
        let mut buf = vec![0f32; resampled.len()];
        let _ = consumer.pop(&mut buf);
    }
    let ring_us = t.elapsed().as_secs_f64() * 1e6 / reps as f64;

    // 4. segmenter push of one 30ms frame (480 samples), the pipeline's
    //    per-frame cost including the Partial{audio: frame.to_vec()} alloc.
    let frame30: Vec<f32> = (0..480).map(|i| 0.3 * (i as f32 * 0.2).sin()).collect();
    let mut seg = audio::segment::SpeechSegmenter::new(
        audio::vad::EnergyVad::from_sensitivity(50),
        audio::segment::SegmenterConfig::default(),
    );
    let t = Instant::now();
    for _ in 0..reps {
        let evs = seg.push(&frame30);
        sink += evs.len() as f32;
    }
    let seg_us = t.elapsed().as_secs_f64() * 1e6 / reps as f64;

    // 5. the f32 -> little-endian byte conversion the Apple backend does on
    //    every fed chunk (asr/backends/apple.rs feed()).
    let t = Instant::now();
    for _ in 0..reps {
        let mut bytes = Vec::with_capacity(frame30.len() * 4);
        for s in &frame30 {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        sink += bytes[0] as f32;
    }
    let le_us = t.elapsed().as_secs_f64() * 1e6 / reps as f64;

    println!("per capture callback (512 frames stereo 48k = 10.7ms of audio):");
    println!("  downmix           {downmix_us:8.2} us");
    println!("  resample 48->16k  {resample_us:8.2} us");
    println!("  ring push+pop     {ring_us:8.2} us");
    println!("per 30ms segmenter frame (480 samples = 30ms of audio):");
    println!("  segmenter.push    {seg_us:8.2} us");
    println!("  f32->LE bytes     {le_us:8.2} us");
    let per_frame = seg_us + le_us;
    println!(
        "\ncapture chain per 10.7ms callback: {:.2} us ({:.4}% of realtime)",
        downmix_us + resample_us + ring_us,
        (downmix_us + resample_us + ring_us) / 10_700.0 * 100.0
    );
    println!(
        "recognizer chain per 30ms frame:   {per_frame:.2} us ({:.4}% of realtime)",
        per_frame / 30_000.0 * 100.0
    );
    println!("(sink {sink:e})");
}
