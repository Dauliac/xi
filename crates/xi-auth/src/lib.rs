//! `xi auth` — thin wrapper around [`nix-auth`] for managing Nix
//! `access-tokens` via OAuth device flow.
//!
//! [`nix-auth`]: https://github.com/numtide/nix-auth

pub mod args;
mod auth;

pub use auth::run;
