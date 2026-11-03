//! Pull-GitOps daemon for `xi sources`.
//!
//! The daemon watches a remote, fast-forwards the sources dir when it
//! advances, and runs operator hooks around the result. The modules:
//!
//! - [`singleton`]  -  one scheduler per `SourcesId`, so two ticks can never
//!   race on the same sources dir.
//! - [`sentinel`]  -  the `.xi-managed` header, and the write guards that
//!   refuse to touch a directory xi does not own.
//! - [`health`]  -  the degraded-reason taxonomy and per-tick outcome.
//! - [`exec`]  -  the subprocess seam for everything *except* git (profile
//!   registration, hooks), so those steps are driven against a recorder in
//!   tests instead of the real world.
//! - [`fetch`]  -  `git fetch` and the fast-forward checkout, direct subprocess
//!   calls so `~/.gitconfig` is honoured.
//! - [`signing`]  -  SSH (`allowed_signers`) and GPG (`gpg_public_keys`) tip
//!   signature verification.
//! - [`machine_id`]  -  the activation-time guard comparing a generation's
//!   embedded host id against the machine it is about to run on.
//! - [`hooks`]  -  `on_advance` / `on_build` / `on_switch` shell command
//!   execution, with output captured to the state dir.
//! - [`profile`]  -  the named Nix profile the daemon registers generations on,
//!   and the `nix-env` invocation that does it.
//! - [`tick`]  -  the orchestrator: fetch, verify, resolve, and the
//!   `on_advance` hook, tying the modules above into one `TickOutcome`.
//!
//! Realising a generation and activating it (`xi-sources-gitops` section
//! 6.2 and the machine-id-gated activation of section 7.2) are not written
//! yet; [`tick::run`] stops once the sources dir has advanced or been
//! refused. See the `xi-sources-gitops` change for the remaining work.

pub mod exec;
pub mod fetch;
pub mod health;
pub mod hooks;
pub mod machine_id;
pub mod profile;
pub mod sentinel;
pub mod signing;
pub mod singleton;
pub mod tick;
