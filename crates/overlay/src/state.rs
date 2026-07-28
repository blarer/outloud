//! The product state machine as the overlay renders it.
//!
//! This is a direct transcription of the state diagram and state-by-state
//! contract in `docs/ux/05-settings-and-states.md`. One machine is shared by
//! every rendering surface (GUI overlay, tray glyph, OSC cursor color, tmux
//! widget, `hexa status --json`), so this enum lives in the
//! platform-independent half of the crate and carries the *contract* for each
//! state — visibility, label — not just the name. Keeping the transition
//! table here, rather than implicit in engine code, is what lets a unit test
//! prove the machine has no absorbing states (UX principle 4).

use std::fmt;

/// The eight product states from `docs/ux/05-settings-and-states.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverlayState {
    /// Model resident, waiting for the hotkey. Overlay invisible.
    Idle,
    /// Mic hot. Waveform + partial tail.
    Listening,
    /// Key released, recognizer finalizing. "…" with elapsed ms if slow.
    Transcribing,
    /// Writing into the target field. Invisible: it is ~13–47ms per M0, and
    /// celebrating routine success is an interruption (UX principle 1).
    Injecting,
    /// Something failed after all fallbacks. One line: situation → action.
    Error,
    /// Accessibility (or mic) permission missing or revoked.
    NoPermission,
    /// Model paging in. The hotkey still works: audio buffers now,
    /// transcribes when ready, and the overlay says so.
    ModelLoading,
    /// Optional network features unreachable. Core dictation unaffected, so
    /// the overlay shows nothing proactive.
    DegradedOffline,
}

impl OverlayState {
    /// Every state, in a stable order. Used by exhaustive tests and by the
    /// demo binary that cycles through all of them.
    pub const ALL: [OverlayState; 8] = [
        OverlayState::Idle,
        OverlayState::Listening,
        OverlayState::Transcribing,
        OverlayState::Injecting,
        OverlayState::Error,
        OverlayState::NoPermission,
        OverlayState::ModelLoading,
        OverlayState::DegradedOffline,
    ];

    /// Whether the overlay surface shows anything at all in this state.
    ///
    /// Invisible-by-default is the first UX principle: `Idle`, `Injecting`,
    /// and `DegradedOffline` render nothing, per the state table.
    pub fn overlay_visible(self) -> bool {
        !matches!(
            self,
            OverlayState::Idle | OverlayState::Injecting | OverlayState::DegradedOffline
        )
    }

    /// The default one-word (or short) label a surface shows when the host
    /// supplies no state-specific detail string.
    pub fn label(self) -> &'static str {
        match self {
            OverlayState::Idle => "idle",
            OverlayState::Listening => "listening",
            OverlayState::Transcribing => "transcribing…",
            OverlayState::Injecting => "inserting",
            OverlayState::Error => "error",
            OverlayState::NoPermission => "permission needed",
            OverlayState::ModelLoading => "loading model…",
            OverlayState::DegradedOffline => "offline",
        }
    }

    /// The transitions out of this state, exactly as drawn in the state
    /// diagram in `docs/ux/05-settings-and-states.md`. The overlay does not
    /// drive transitions (the engine does), but encoding the table here lets
    /// tests enforce the two structural guarantees the docs promise:
    /// no absorbing states, and every state reachable from launch.
    pub fn allowed_transitions(self) -> &'static [OverlayState] {
        use OverlayState::*;
        match self {
            ModelLoading => &[Idle, NoPermission, Error],
            Idle => &[Listening, NoPermission, ModelLoading, DegradedOffline],
            Listening => &[Transcribing, Idle, Error],
            Transcribing => &[Injecting, Idle, Error],
            Injecting => &[Idle, Error],
            NoPermission => &[Idle],
            Error => &[Idle],
            DegradedOffline => &[Idle],
        }
    }

    /// True when `next` is a legal transition from `self`.
    pub fn can_transition_to(self, next: OverlayState) -> bool {
        self.allowed_transitions().contains(&next)
    }
}

impl fmt::Display for OverlayState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The kebab-case names match `hexa status --json` so every surface
        // speaks the same vocabulary.
        let s = match self {
            OverlayState::Idle => "idle",
            OverlayState::Listening => "listening",
            OverlayState::Transcribing => "transcribing",
            OverlayState::Injecting => "injecting",
            OverlayState::Error => "error",
            OverlayState::NoPermission => "no-permission",
            OverlayState::ModelLoading => "model-loading",
            OverlayState::DegradedOffline => "degraded-offline",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::OverlayState::{self, *};
    use std::collections::HashSet;

    #[test]
    fn no_absorbing_states() {
        // UX principle 4: every error-shaped state has a named exit.
        for s in OverlayState::ALL {
            assert!(
                !s.allowed_transitions().is_empty(),
                "{s} has no way out — absorbing states are forbidden by the UX contract"
            );
        }
    }

    #[test]
    fn every_state_reachable_from_launch() {
        // Launch enters ModelLoading; BFS from there must cover the machine,
        // otherwise a state exists that no user can ever see.
        let mut seen = HashSet::from([ModelLoading]);
        let mut queue = vec![ModelLoading];
        while let Some(s) = queue.pop() {
            for &n in s.allowed_transitions() {
                if seen.insert(n) {
                    queue.push(n);
                }
            }
        }
        assert_eq!(seen.len(), OverlayState::ALL.len(), "unreachable state(s)");
    }

    #[test]
    fn transitions_match_the_ux_doc_exactly() {
        // Spot-check the edges the docs call out as deliberate decisions.
        assert!(Idle.can_transition_to(Listening));
        assert!(Listening.can_transition_to(Idle), "Esc cancel must work");
        assert!(
            Transcribing.can_transition_to(Idle),
            "empty (silence) result returns to idle"
        );
        assert!(
            !Injecting.can_transition_to(Listening),
            "no shortcut edges the diagram does not draw"
        );
        assert!(!Idle.can_transition_to(Transcribing));
        assert!(NoPermission.can_transition_to(Idle));
    }

    #[test]
    fn visibility_matches_the_state_table() {
        // The state table: idle/injecting/degraded-offline show nothing.
        let invisible: Vec<_> = OverlayState::ALL
            .iter()
            .filter(|s| !s.overlay_visible())
            .collect();
        assert_eq!(invisible, [&Idle, &Injecting, &DegradedOffline]);
    }

    #[test]
    fn display_names_are_kebab_case_json_vocabulary() {
        assert_eq!(NoPermission.to_string(), "no-permission");
        assert_eq!(ModelLoading.to_string(), "model-loading");
        assert_eq!(DegradedOffline.to_string(), "degraded-offline");
    }
}
