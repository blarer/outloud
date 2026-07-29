//! `outloud`: the daemon that turns the individually-proven crates into a
//! product. Hold the hotkey, speak, release, text appears; hold with a
//! selection, speak an edit command, the selection is rewritten.
//!
//! ## The wiring (deliverable 1)
//!
//! ```text
//! hotkey (CGEventTap thread)                 [std mpsc]
//!    └─ bridge task ──────────────────────► supervisor (tokio)
//! cpal capture (realtime thread)
//!    └─ ring buffer (drop-oldest) ─────────► drain task (30ms tick)
//!         └─ SpeechSegmenter (VAD) ────────► recognizer thread (bounded
//!               SpeechStart/Partial audio     sync_channel, try_send:
//!                                             overrun DROPS audio and
//!                                             counts it, never blocks)
//! recognizer finalize ──► edit-intent parse ──► ax-edit / text-target write
//!                                          └──► overlay frame (Arc<Mutex>,
//!                                               store-only, never blocks)
//! ```
//!
//! Backpressure policy, stated once and enforced twice: audio is *dropped,
//! never awaited*. The ring buffer drops oldest on overrun (a recognizer
//! that is behind is useless for dictation), and the recognizer channel
//! drops newest with a counter (an honest gap beats a wedged event tap).
//! Nothing on the capture or event-tap side ever waits on a consumer.
//!
//! ## Ownership of the main thread
//!
//! AppKit demands the overlay live on the main thread, and the pipeline must
//! never block on rendering. So the daemon inverts the usual layout: the
//! *overlay* owns the main thread (an NSTimer polling a shared frame at
//! 30Hz), and the whole tokio pipeline runs on a background thread. When
//! there is no display (or `--no-overlay`), the pipeline gets the main
//! thread and state transitions are logged instead of drawn.

pub mod ax_stream;
pub mod devlatency;
pub mod freeform;
pub mod inject;
pub mod instance;
pub mod menubar;
pub mod menuhost;
pub mod mic;
pub mod pipeline;
pub mod recognize;
pub mod runtime;
pub mod source;
pub mod state;
pub mod streamer;
pub mod wav;

/// Convert a UTF-16 code-unit offset (the unit the accessibility API
/// reports selections in) to a byte offset into `s`.
///
/// Returns `None` when the offset is out of range or lands inside a
/// surrogate pair, because splicing at a wrong boundary corrupts the user's
/// document, and refusing is recoverable while corrupting is not.
pub fn utf16_offset_to_byte(s: &str, utf16_offset: usize) -> Option<usize> {
    if utf16_offset == 0 {
        return Some(0);
    }
    let mut units = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if units == utf16_offset {
            return Some(byte_idx);
        }
        units += ch.len_utf16();
        if units > utf16_offset {
            return None; // inside a surrogate pair
        }
    }
    (units == utf16_offset).then_some(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_offsets_are_identity() {
        assert_eq!(utf16_offset_to_byte("hello", 0), Some(0));
        assert_eq!(utf16_offset_to_byte("hello", 3), Some(3));
        assert_eq!(utf16_offset_to_byte("hello", 5), Some(5));
        assert_eq!(utf16_offset_to_byte("hello", 6), None);
    }

    #[test]
    fn multibyte_chars_convert_correctly() {
        // é is 1 UTF-16 unit but 2 bytes.
        let s = "caf\u{e9} time";
        assert_eq!(utf16_offset_to_byte(s, 4), Some(5));
    }

    #[test]
    fn surrogate_pair_interior_is_refused() {
        // 𝄞 (U+1D11E) is 2 UTF-16 units, 4 bytes.
        let s = "a\u{1D11E}b";
        assert_eq!(utf16_offset_to_byte(s, 1), Some(1));
        assert_eq!(utf16_offset_to_byte(s, 2), None, "mid-surrogate");
        assert_eq!(utf16_offset_to_byte(s, 3), Some(5));
    }
}
