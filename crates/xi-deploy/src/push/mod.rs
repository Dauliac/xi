//! The `xi push` verb: evaluate here, apply there.
//!
//! `xi push` drives a *fleet*, so the capability is split along the seams
//! the `xi-sources-push` spec cares about rather than by Nix-versus-SSH:
//!
//! - [`args`]  -  the command surface, including the backend and mode enums the
//!   other modules dispatch on.
//! - [`discover`]  -  which targets exist, read off the flake output taxonomy,
//!   and which backend each one wants.
//! - [`preflight`]  -  the four per-target checks that run before anything
//!   mutates a target, reported per target rather than fail-fast.
//! - [`limits`]  -  the independent eval and apply concurrency caps, so a slow
//!   evaluation cannot serialise the fleet.
//! - [`engine`]  -  the two side-effecting traits ([`engine::Nix`],
//!   [`engine::Transport`]) that every mutation goes through, so the
//!   orchestration above is testable without real hosts.
//! - [`rollback`]  -  the magic-rollback canary loop: plant a sentinel
//!   and an escape hatch after activation, poll over SSH, and trigger
//!   the escape hatch if the sentinel goes missing within the window.
//!
//! The orchestrator that ties these together, and the CLI wiring that
//! mounts the verb, are not written yet. See the `xi-sources-push`
//! change for the remaining work.

pub mod args;
pub mod discover;
pub mod engine;
pub mod limits;
pub mod preflight;
pub mod rollback;
