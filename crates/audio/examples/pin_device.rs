//! List capture devices, then (optionally) open a stream pinned to the
//! device named by the first CLI argument, proving `microphone = "..."`
//! selection against real hardware.
//!
//! Run: `cargo run -p audio --example pin_device --features capture -- blackshark`

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
    println!("\npinning to \"{want}\" for 3 seconds...");
    let (producer, consumer) = audio::ring::ring(audio::SAMPLE_RATE as usize * 2);
    let handle = audio::capture::start_capture_preferring(producer, Some(want), |ev| {
        println!("  event: {ev:?}");
    });
    std::thread::sleep(std::time::Duration::from_secs(3));
    let mut buf = vec![0f32; audio::SAMPLE_RATE as usize * 2];
    let n = consumer.pop(&mut buf);
    let peak = buf[..n].iter().fold(0f32, |m, s| m.max(s.abs()));
    println!("  captured {n} samples, peak {peak:.4}");
    handle.stop();
    Ok(())
}
