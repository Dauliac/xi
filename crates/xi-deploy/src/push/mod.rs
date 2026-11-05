//! The `xi push` verb: evaluate here, apply there.
//!
//! `xi push` drives a *fleet*, so the capability is split along the seams
//! the `xi-sources-push` spec cares about rather than by Nix-versus-SSH:
//!
//! - [`args`]  -  the command surface, including the backend and mode enums the
//!   other modules dispatch on.
//! - [`context`]  -  resolves the sources context a push deploys from, and
//!   refuses before dispatch if the scope has no sources configured.
//! - [`config`]  -  the `.xi.toml [push]` table: the declarative
//!   `[[push.hosts]]` inventory and the `remote-build` config rung.
//! - [`discover`]  -  which targets exist, read off the flake output taxonomy,
//!   and which backend each one wants.
//! - [`preflight`]  -  the four per-target checks that run before anything
//!   mutates a target, reported per target rather than fail-fast.
//! - [`limits`]  -  the independent eval and apply concurrency caps, so a slow
//!   evaluation cannot serialise the fleet.
//! - [`engine`]  -  the two side-effecting traits ([`engine::Nix`],
//!   [`engine::Transport`]) that every mutation goes through, so the
//!   orchestration above is testable without real hosts.
//! - [`rollback`]  -  the magic-rollback canary loop: plant a sentinel and an
//!   escape hatch after activation, poll over SSH, and trigger the escape hatch
//!   if the sentinel goes missing within the window.
//! - [`mode_sources`]  -  `--mode sources`: rsync the sources dir to the
//!   target's scope-resolved path instead of shipping a built closure.
//! - [`orchestrate`]  -  ties every module above into the `xi push` verb:
//!   resolve context, discover, select, pick a backend, dispatch.

pub mod args;
pub mod config;
pub mod context;
pub mod discover;
pub mod engine;
pub mod limits;
pub mod mode_sources;
pub mod orchestrate;
pub mod preflight;
pub mod rollback;
