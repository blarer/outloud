//! Concrete [`TextTarget`](crate::TextTarget) implementations, one module
//! per tier.

// Terminal and headless transports are always available: they are exactly
// the ones that work without a graphical session.
pub mod headless;
pub mod terminal;

// The remaining tiers need a display server, so a headless build must not
// even compile them. See the `display` feature in Cargo.toml.
#[cfg(feature = "display")]
pub mod ax;
#[cfg(feature = "display")]
pub mod clipboard;
#[cfg(feature = "display")]
pub mod ime;
#[cfg(feature = "display")]
pub mod keys;
