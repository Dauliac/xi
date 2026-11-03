//! The `xi-sources-observability` capability.
//!
//! An operator cannot debug a daemon they cannot see. This crate owns the
//! four surfaces through which the `xi-sources-*` family becomes visible,
//! and nothing else:
//!
//! A module named in backticks below is part of the target shape but is
//! not written yet; only the linked ones exist. See *Implementation
//! status*.
//!
//! - **structured logs** - [`fields`] fixes the span vocabulary,
//!   [`subscriber`] builds the ndjson tracing stack, [`rotating`] is the
//!   user-scope daily-rotating sink, [`journald`] is the system-scope one,
//!   `readout` reads either sink back.
//! - **events** - [`events`] is a tiny sqlite store that outlives the log
//!   it describes, queried by `xi <scope> sources events`.
//! - **metrics** - [`metrics`] is the closed Prometheus set and
//!   [`server`] exposes it on loopback alongside `health`.
//! - **traces** - `otlp` batches tick root spans out to a collector when
//!   (and only when) an operator names one.
//!
//! [`redact`] cuts across all four: the credential type
//! (`xi_core::sources::credentials::Secret`) has no `Serialize` impl,
//! free text is scrubbed on the way to a sink, and [`redact::lint`]
//! scans the tree for log calls that try to smuggle one through anyway.
//!
//! # Crate layout and features
//!
//! The daemon-side machinery (tracing layers, exporter, metric
//! registry) sits behind the default `daemon` feature. Readers depend on
//! this crate with `default-features = false`, which leaves the field
//! vocabulary, the event store, the readouts, and the redaction
//! boundary - everything the CLI needs and nothing it does not. `otlp`
//! is a second, non-default feature on top of `daemon`: OTLP export is
//! an operator opt-in even for a daemon build, so its client stack
//! (`opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`,
//! `tracing-opentelemetry`) only links in when a caller asks for it.
//!
//! # Daemon wiring seams
//!
//! The gitops daemon is a separate capability. The three places it
//! plugs in are `subscriber::init` (once at startup),
//! [`fields::TickSpan`] / [`fields::PhaseSpan`] (once per tick and
//! phase), and `health::HealthState` (handed to `server::Exporter`
//! so a probe reflects the live loop). Nothing in this crate starts a
//! loop of its own.
//!
//! # Implementation status
//!
//! Ten of the modules the layout above describes exist: [`config`],
//! [`events`], [`fields`], [`redact`], [`rotating`], the `daemon`-gated
//! [`subscriber`], [`journald`], [`metrics`] and [`server`], and the
//! `otlp`-gated `otlp` (off by default; see "Crate layout and
//! features"). The rest - `health`, `readout`, and the `daemon`-gated
//! `panic_hook` - are still to be written, and are declared here only
//! once their file lands. [`server`] today only answers `/metrics`;
//! `/healthz` and the "nothing else" closed-route guarantee are task
//! 5.3, not yet landed. The doc above is the target shape, not a
//! description of what compiles today; see the
//! `xi-sources-observability` change for the remaining work.

pub mod config;
pub mod events;
pub mod fields;
pub mod redact;
pub mod rotating;

#[cfg(all(feature = "daemon", unix))]
pub mod journald;
#[cfg(feature = "daemon")]
pub mod metrics;
#[cfg(feature = "otlp")]
pub mod otlp;
#[cfg(feature = "daemon")]
pub mod server;
#[cfg(feature = "daemon")]
pub mod subscriber;

pub use config::{LogLevel, ObservabilityConfig};
pub use fields::{Outcome, Phase, PhaseSpan, TickSpan};
pub use redact::Scrubber;
