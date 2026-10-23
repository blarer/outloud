//! List capture devices, then (optionally) open a stream pinned to the
//! device named by the first CLI argument, proving `microphone = "..."`
//! selection against real hardware.
//!
//! Run: `cargo run -p audio --example pin_device --features capture -- blackshark [SECONDS]`
//!
//! With a duration, prints one peak/RMS line per second, so "is my voice
//! reaching this input, and how loud" can be answered while talking.

fn main() -> anyhow::Result<()> {
    let devices = audio::capture::input_devices()?;
    println!("capture devices:");
    for d in &devices {
        println!(
            "  {}{}",
            d.name,
            if d.is_default { "  [default]" } else { "" }
        );
    }

    let Some(want) = std::env::args().nth(1) else {
        return Ok(());
    };
    let secs: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    println!("\npinning to \"{want}\" for {secs} seconds...");
    let (producer, consumer) = audio::ring::ring(audio::SAMPLE_RATE as usize * 2);
    let handle = audio::capture::start_capture_preferring(producer, Some(want.clone()), |ev| {
        println!("  event: {ev:?}");
    });
    let mut buf = vec![0f32; audio::SAMPLE_RATE as usize * 2];
    let mut overall_peak = 0f32;
    let mut total = 0usize;
    for s in 1..=secs {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let n = consumer.pop(&mut buf);
        total += n;
        let chunk = &buf[..n];
        let peak = chunk.iter().fold(0f32, |m, x| m.max(x.abs()));
        let rms = if n > 0 {
            (chunk.iter().map(|x| x * x).sum::<f32>() / n as f32).sqrt()
        } else {
            0.0
        };
        let db = if peak > 0.0 {
            20.0 * peak.log10()
        } else {
            -120.0
        };
        overall_peak = overall_peak.max(peak);
        println!("  [{want}] t={s:>2}s peak {peak:.4} ({db:>6.1} dBFS) rms {rms:.5}");
    }
    println!("  [{want}] captured {total} samples, overall peak {overall_peak:.4}");
    handle.stop();
    Ok(())
}
