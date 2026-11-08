//! `xi-vm-lifecycle`: declarative multi-VM dev-space management.
//!
//! "Vagrant / docker-compose, but for NixOS machines, inside xi." See
//! `openspec/changes/xi-vm-lifecycle/{proposal,design}.md` for the full
//! design, and `docs/guides/vm.md` for the operator-facing workflow.
//!
//! Module map:
//! - [`args`][]: the `xi vm` CLI surface.
//! - [`manifest`][]: parses the `xi-vm-manifest.json`
//!   `modules/flake-parts/vm.nix` renders.
//! - [`attrs`][]: the flake attribute naming convention that module uses,
//!   so the heavy build/check derivations can be built directly.
//! - [`build`][]: `build`/`check`.
//! - [`port`][]: runtime host-port resolution.
//! - [`registry`][]: the per-VM running-instance record (see its header
//!   comment for how this differs from the control-socket design the
//!   proposal describes).
//! - [`ssh_key`][]: the xi-managed throwaway SSH keypair.
//! - [`runtime`][]: `start`/`stop`/`connect`/`status`.
//! - [`vm`][]: top-level dispatch, called from `crates/xi/src/interface.rs`.

pub mod args;
pub mod attrs;
pub mod build;
pub mod manifest;
pub mod port;
pub mod registry;
pub mod runtime;
pub mod ssh_key;
pub mod vm;
