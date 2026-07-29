//! What the segmenter does with a recording at each dial position.
//!
//! Exists because a sweep of the whole binary showed no difference between
//! sensitivity settings, which is either the dial not being wired or the
//! recording being rejected somewhere else entirely. This isolates the
//! segmenter so the answer is unambiguous.

fn main() {
    let path = std::env::args().nth(1).expect("usage: vad_probe <wav>");
    let s = outloud::wav::load_16k_mono(std::path::Path::new(&path)).unwrap();
    let rms = (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    println!("file: {} samples, overall RMS {rms:.5}", s.len());

    for sens in [25u8, 50, 75, 100] {
        let mut seg = audio::segment::SpeechSegmenter::new(
            audio::vad::EnergyVad::from_sensitivity(sens),
            audio::segment::SegmenterConfig::default(),
        );
        let mut fed = 0usize;
        let mut ends = 0usize;
        for c in s.chunks(3200) {
            for ev in seg.push(c) {
                use audio::segment::SpeechEvent::*;
                match ev {
                    SpeechStart { audio } | Partial { audio } => fed += audio.len(),
                    SpeechEnd { .. } => ends += 1,
                }
            }
        }
        println!(
            "  sens {sens:3}: knee {:.5}  fed {fed} samples, {ends} endpoints",
            audio::vad::EnergyVad::from_sensitivity(sens).knee()
        );
    }
}
