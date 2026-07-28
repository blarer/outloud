//! Live probe for the streaming write path: opens an `AxRegion` on the
//! focused field and streams a sentence into it the way the daemon would,
//! timing every write.
//!
//! Run with a text field focused (e.g. TextEdit):
//!     cargo run --release -p outloud --example stream_probe
//!
//! This exists because the streaming path cannot be exercised in CI (it
//! needs a live accessibility server) and every claim about its cost needs
//! a number from a real target.

fn main() {
    let snap = match ax_edit::snapshot_focused() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("no focused text field ({e}); focus TextEdit and rerun");
            std::process::exit(1);
        }
    };
    println!(
        "target: {} in {:?} (selected_text_settable={})",
        snap.role, snap.app, snap.selected_text_settable
    );
    let mut region = match outloud::ax_stream::AxRegion::begin(&snap) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("field not streamable: {e:?}");
            std::process::exit(1);
        }
    };

    // The exact command sequence a horizon at stability 2 produces for a
    // revising recognizer, plus the final settle splice.
    let steps: Vec<stream::WriteCommand> = vec![
        stream::WriteCommand::Append("the quick".into()),
        stream::WriteCommand::Append(" brown fox".into()),
        stream::WriteCommand::Append(" jumps over".into()),
        stream::WriteCommand::Splice {
            range: 4..9,
            insert: "slick".into(),
        },
        stream::WriteCommand::Append(" the lazy dog.".into()),
    ];
    let mut times = Vec::new();
    for cmd in &steps {
        let t0 = std::time::Instant::now();
        match region.apply(cmd) {
            Ok(()) => times.push(t0.elapsed()),
            Err(e) => {
                eprintln!("write failed: {e}");
                std::process::exit(1);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(80)); // the coalescer cadence
    }
    let t0 = std::time::Instant::now();
    region.seal().expect("seal");
    let seal = t0.elapsed();

    times.sort();
    println!(
        "writes: n={} min={:?} p50={:?} max={:?}; seal={:?}",
        times.len(),
        times[0],
        times[times.len() / 2],
        times[times.len() - 1],
        seal
    );
}
