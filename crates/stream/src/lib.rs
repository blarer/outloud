//! The streaming text commit layer.
//!
//! A streaming recognizer revises its hypothesis: "recognise speech" can
//! become "wreck a nice beach" three words later. If the first version was
//! already injected into the user's document, every correction is visible.
//! This crate decides *what* is safe to write, *when*, and *how*, so that:
//!
//! - committed text is never retracted ([`horizon::CommitHorizon`]),
//! - each write is the minimal word-level splice, boundary-aware for CJK
//!   and grapheme-safe for emoji/combining marks ([`diff::minimal_edit`]),
//! - writes never exceed ~1 per 80ms and slow transports drop stale
//!   intermediates instead of queueing them ([`coalesce::Coalescer`]),
//! - transports that cannot revise text (`can_write_in_place: false`)
//!   automatically degrade to a single commit-on-release write
//!   ([`session::DictationSession`]),
//! - one dictation is one undo step, with a stale-snapshot guard
//!   ([`undo::UndoRing`]).
//!
//! Everything is pure logic over injected time and explicit commands, so
//! all of it runs and is property-tested with no OS integration at all.
//! The narrative documentation, worked examples, degradation matrix, and
//! failure-mode catalogue live in `docs/streaming.md`.

pub mod coalesce;
pub mod diff;
pub mod horizon;
pub mod session;
pub mod undo;

pub use coalesce::{Coalescer, DEFAULT_WRITE_INTERVAL};
pub use diff::{minimal_edit, Edit};
pub use horizon::{CommitHorizon, HorizonConfig, HorizonUpdate};
pub use session::{
    seal_undo, DeliveryMode, DictationSession, OverlayState, SessionUpdate, TransportProfile,
    WriteCommand,
};
pub use undo::{UndoOutcome, UndoRing, UndoUnit};
