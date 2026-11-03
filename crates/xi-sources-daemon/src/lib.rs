//! Pull-GitOps daemon for `xi sources`.
//!
//! The daemon watches a remote, fast-forwards the sources dir when it
//! advances, and activates the result. This crate currently carries the
//! four pieces that are independent of the tick loop itself:
//!
//! - [`singleton`]  -  one scheduler per `SourcesId`, so two ticks can never
//!   race on the same sources dir.
//! - [`sentinel`]  -  the `.xi-managed` header, and the write guards that
//!   refuse to touch a directory xi does not own.
//! - [`health`]  -  the degraded-reason taxonomy and per-tick outcome.
//! - [`exec`]  -  the subprocess seam, so the tick loop can be driven against a
//!   recorder in tests instead of a real `git`.
//!
//! The tick loop and the `xi-sources-daemon` binary that drives it are not
//! written yet; the crate is a library until they are. See the
//! `xi-sources-gitops` change for the remaining work.

pub mod exec;
pub mod health;
pub mod sentinel;
pub mod singleton;
