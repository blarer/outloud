//! Engine-side state: drives the eight documented product states and hands
//! the overlay a frame to draw, without ever letting the overlay block the
//! pipeline.
//!
//! `overlay::OverlayState` already encodes the state diagram and its legal
//! transitions (docs/ux/05-settings-and-states.md). This module adds the two
//! things the daemon needs on top:
//!
//! - **Transition enforcement.** The engine only ever moves along edges the
//!   diagram draws; an illegal transition is a bug and is logged loudly (not
//!   panicked: the daemon must keep dictating even if a state edge is wrong).
//! - **A non-blocking publication channel.** The pipeline *stores* the
//!   latest frame under a mutex held for nanoseconds; the overlay thread
//!   *polls* it at ~30Hz. The pipeline never waits for a render, and a hung
//!   render loop can at worst show a stale frame, never stall dictation.

use std::sync::{Arc, Mutex};

use overlay::{OverlayFrame, OverlayState};

/// Shared slot the pipeline writes and the render loop reads. Storing a
/// whole frame (not deltas) matches the overlay's own contract: "what is
/// visible" is a pure function of the last frame.
#[derive(Clone)]
pub struct StatusShared {
    frame: Arc<Mutex<OverlayFrame>>,
}

impl StatusShared {
    /// Read the latest frame. Called from the render thread.
    pub fn snapshot(&self) -> OverlayFrame {
        self.frame.lock().expect("status lock poisoned").clone()
    }
}

/// The engine's view: owns the current state, enforces the transition
/// table, publishes frames.
pub struct Engine {
    state: OverlayState,
    shared: StatusShared,
    /// Live audio level, refreshed independently of state changes so the
    /// waveform animates between transitions.
    level: f32,
    /// Partial text tail while listening/transcribing.
    partial: String,
    /// True once an illegal transition was attempted; exposed for tests.
    saw_illegal: bool,
}

impl Engine {
    pub fn new() -> (Engine, StatusShared) {
        // Launch enters ModelLoading per the state diagram: the recognizer
        // may need to spawn a helper or download an OS model asset.
        let initial = OverlayFrame::state_only(OverlayState::ModelLoading);
        let shared = StatusShared {
            frame: Arc::new(Mutex::new(initial)),
        };
        (
            Engine {
                state: OverlayState::ModelLoading,
                shared: shared.clone(),
                level: 0.0,
                partial: String::new(),
                saw_illegal: false,
            },
            shared,
        )
    }

    pub fn state(&self) -> OverlayState {
        self.state
    }

    /// Move to `next`, carrying an optional detail line. Illegal edges are
    /// refused and logged: the previous state stays, so the UI never shows a
    /// state the machine cannot actually be in.
    pub fn transition(&mut self, next: OverlayState, detail: Option<String>) {
        if self.state != next && !self.state.can_transition_to(next) {
            eprintln!(
                "aquad: BUG: illegal state transition {} -> {} (staying in {})",
                self.state, next, self.state
            );
            self.saw_illegal = true;
            return;
        }
        self.state = next;
        // Entering a state clears per-utterance data unless the caller is
        // continuing one (Listening -> Transcribing keeps the partial tail
        // so the user sees what will be committed).
        if matches!(next, OverlayState::Idle | OverlayState::Listening) {
            self.partial.clear();
        }
        if next != OverlayState::Listening {
            self.level = 0.0;
        }
        self.publish(detail);
    }

    /// Update live listening data without a state change.
    pub fn live(&mut self, level: f32, partial: Option<&str>) {
        self.level = level.clamp(0.0, 1.0);
        if let Some(p) = partial {
            self.partial = p.to_string();
        }
        self.publish(None);
    }

    pub fn saw_illegal_transition(&self) -> bool {
        self.saw_illegal
    }

    fn publish(&self, detail: Option<String>) {
        let frame = OverlayFrame {
            state: self.state,
            audio_level: self.level,
            partial_text: self.partial.clone(),
            detail,
            // The corner fallback: caret-anchoring needs AXBoundsForRange,
            // which ax-edit does not expose yet (reported as needed work).
            anchor: overlay::Anchor::Corner,
        };
        *self.shared.frame.lock().expect("status lock poisoned") = frame;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use OverlayState::*;

    #[test]
    fn happy_path_walks_the_diagram() {
        let (mut e, shared) = Engine::new();
        for s in [Idle, Listening, Transcribing, Injecting, Idle] {
            e.transition(s, None);
            assert_eq!(e.state(), s);
        }
        assert!(!e.saw_illegal_transition());
        assert_eq!(shared.snapshot().state, Idle);
    }

    #[test]
    fn illegal_transition_is_refused_not_taken() {
        let (mut e, _s) = Engine::new();
        e.transition(Idle, None);
        e.transition(Transcribing, None); // Idle -> Transcribing not drawn
        assert_eq!(e.state(), Idle);
        assert!(e.saw_illegal_transition());
    }

    #[test]
    fn error_paths_have_exits() {
        let (mut e, _s) = Engine::new();
        e.transition(Idle, None);
        e.transition(Listening, None);
        e.transition(Error, Some("mic stream died -> reconnecting".into()));
        e.transition(Idle, None); // named exit per UX principle 4
        assert_eq!(e.state(), Idle);
        assert!(!e.saw_illegal_transition());
    }

    #[test]
    fn live_updates_do_not_change_state() {
        let (mut e, shared) = Engine::new();
        e.transition(Idle, None);
        e.transition(Listening, None);
        e.live(0.7, Some("hello wor"));
        let f = shared.snapshot();
        assert_eq!(f.state, Listening);
        assert_eq!(f.partial_text, "hello wor");
        assert!((f.audio_level - 0.7).abs() < 1e-6);
    }

    #[test]
    fn leaving_listening_resets_level_keeps_partial() {
        let (mut e, shared) = Engine::new();
        e.transition(Idle, None);
        e.transition(Listening, None);
        e.live(0.9, Some("hello world"));
        e.transition(Transcribing, None);
        let f = shared.snapshot();
        assert_eq!(f.audio_level, 0.0);
        // Kept so the user sees what is being committed.
        assert_eq!(f.partial_text, "hello world");
    }
}
