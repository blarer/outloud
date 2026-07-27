//! The product's own undo ring.
//!
//! Why it exists: writing `AXValue` resets the host application's undo
//! stack (README, docs/ux/02-core-interaction.md), so after a streamed
//! dictation the host's Cmd+Z is either dead or wrong. The product
//! therefore keeps its own before/after snapshots and restores through the
//! same write path.
//!
//! The invariant that shapes the API: **one user-visible dictation is one
//! undo step**. A streamed utterance produces dozens of transport writes,
//! but the user performed one action, so the ring records one entry:
//! `begin_unit` captures the before-image once, every streamed write lands
//! inside the open unit, and `end_unit` seals it with the final
//! after-image. Nothing between begin and end is individually undoable.
//!
//! Safety rule, taken verbatim from the UX doc: never blindly stomp user
//! keystrokes with a stale snapshot. `undo` hands back the before-image
//! only if the caller proves the field still contains our after-image;
//! otherwise it refuses and surfaces the before-text for the clipboard
//! path ("The field changed since that dictation. Copied the previous
//! version.").

use std::collections::VecDeque;

/// One sealed dictation: the field's text around a single user action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoUnit {
    /// Field contents before our first write of this dictation.
    pub before: String,
    /// Field contents after our last write of this dictation.
    pub after: String,
    /// Caret byte offset to restore with `before`, when it was known.
    pub caret_before: Option<usize>,
}

/// What `undo` decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoOutcome {
    /// Safe to restore: write `unit.before` through the transport.
    Restore(UndoUnit),
    /// The field no longer holds what we wrote (user typed since). The
    /// before-text is offered for the clipboard fallback instead of being
    /// force-written over the user's newer edits.
    FieldChanged { before: String },
    /// Nothing recorded.
    Empty,
}

/// Fixed-capacity ring of sealed dictation units, newest last. A ring, not
/// a stack that grows forever: dictations are frequent and old
/// before-images of large documents are memory nobody will ever ask for.
#[derive(Debug)]
pub struct UndoRing {
    units: VecDeque<UndoUnit>,
    capacity: usize,
    /// The unit currently being streamed into, not yet undoable: undoing a
    /// dictation *while it is still being spoken* is cancellation's job,
    /// not undo's.
    open: Option<UndoUnit>,
}

impl UndoRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            units: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            open: None,
        }
    }

    /// A dictation is starting: snapshot the field once, before anything is
    /// written. Streamed writes between this and `end_unit` do not touch
    /// the ring. Calling begin twice seals nothing and replaces the open
    /// unit, because two overlapping dictations into one field cannot both
    /// be true (the second one's before-image is the truth of the field now).
    pub fn begin_unit(&mut self, field_text_before: &str, caret_before: Option<usize>) {
        self.open = Some(UndoUnit {
            before: field_text_before.to_string(),
            after: String::new(),
            caret_before,
        });
    }

    /// The dictation finished; `field_text_after` is what the field holds
    /// now. Seals the open unit into the ring as ONE undo step. A unit
    /// that changed nothing is discarded: an undo step that does nothing
    /// would make "undo that" feel broken.
    pub fn end_unit(&mut self, field_text_after: &str) {
        if let Some(mut unit) = self.open.take() {
            unit.after = field_text_after.to_string();
            if unit.before == unit.after {
                return;
            }
            if self.units.len() == self.capacity {
                self.units.pop_front();
            }
            self.units.push_back(unit);
        }
    }

    /// The dictation was cancelled before any commit; drop the open unit.
    pub fn abort_unit(&mut self) {
        self.open = None;
    }

    /// Undo the most recent dictation. `field_text_now` is the field's
    /// current contents, read back just before restoring, which is the
    /// stale-snapshot guard.
    pub fn undo(&mut self, field_text_now: &str) -> UndoOutcome {
        match self.units.pop_back() {
            None => UndoOutcome::Empty,
            Some(unit) => {
                if field_text_now == unit.after {
                    UndoOutcome::Restore(unit)
                } else {
                    // The user (or the app) changed the field after our
                    // write. Restoring would destroy their newer work.
                    UndoOutcome::FieldChanged {
                        before: unit.before,
                    }
                }
            }
        }
    }

    /// How many sealed dictations are undoable.
    pub fn len(&self) -> usize {
        self.units.len()
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_streamed_dictation_is_one_undo_step() {
        let mut ring = UndoRing::new(8);
        ring.begin_unit("existing. ", Some(10));
        // Forty streamed writes happen here; the ring never sees them.
        ring.end_unit("existing. hello world");
        assert_eq!(ring.len(), 1, "many writes, one unit");
        match ring.undo("existing. hello world") {
            UndoOutcome::Restore(u) => {
                assert_eq!(u.before, "existing. ");
                assert_eq!(u.caret_before, Some(10));
            }
            other => panic!("expected restore, got {other:?}"),
        }
        assert!(ring.is_empty());
    }

    #[test]
    fn refuses_to_stomp_user_edits() {
        let mut ring = UndoRing::new(8);
        ring.begin_unit("", None);
        ring.end_unit("dictated text");
        // The user typed after our write; the field no longer matches.
        match ring.undo("dictated text plus typing") {
            UndoOutcome::FieldChanged { before } => assert_eq!(before, ""),
            other => panic!("expected FieldChanged, got {other:?}"),
        }
    }

    #[test]
    fn noop_units_are_not_recorded() {
        let mut ring = UndoRing::new(8);
        ring.begin_unit("same", None);
        ring.end_unit("same");
        assert!(ring.is_empty(), "a do-nothing undo step feels broken");
    }

    #[test]
    fn ring_evicts_oldest_at_capacity() {
        let mut ring = UndoRing::new(2);
        for i in 0..3 {
            ring.begin_unit(&format!("before{i}"), None);
            ring.end_unit(&format!("after{i}"));
        }
        assert_eq!(ring.len(), 2);
        match ring.undo("after2") {
            UndoOutcome::Restore(u) => assert_eq!(u.before, "before2"),
            other => panic!("{other:?}"),
        }
        match ring.undo("after1") {
            UndoOutcome::Restore(u) => assert_eq!(u.before, "before1"),
            other => panic!("{other:?}"),
        }
        assert!(ring.is_empty(), "unit 0 was evicted");
    }

    #[test]
    fn abort_discards_the_open_unit() {
        let mut ring = UndoRing::new(8);
        ring.begin_unit("text", None);
        ring.abort_unit();
        ring.end_unit("changed");
        assert!(ring.is_empty(), "end after abort must not resurrect");
    }

    #[test]
    fn undo_on_empty_ring() {
        let mut ring = UndoRing::new(8);
        assert_eq!(ring.undo("anything"), UndoOutcome::Empty);
    }
}
