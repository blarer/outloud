//! Intentionally empty. This crate exists to carry the workspace-level
//! integration tests in `tests/`, which exercise the seams *between* crates:
//! read -> parse -> apply -> write across edit-intent and text-target,
//! transport selection against simulated environments, error propagation
//! across crate boundaries, and record/replay through diag. Unit tests
//! belong in the crate that owns the logic; only cross-crate behavior
//! belongs here.
