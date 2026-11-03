//! The `machineId` guard: refuse to activate a generation built for a
//! different host.
//!
//! Steals comin's `internal/manager/manager.go:208-209` check: a
//! generation carries the id of the machine it was built on, and
//! activation compares that against the machine it is about to run on.
//! A mismatch is refused unless the operator overrides it explicitly  -
//! the daemon itself never passes that override, because an automated
//! bypass of a safety check is not a safety check.
//!
//! The token differs by scope, per spec:
//!
//! - System scope reads `/etc/machine-id` (the `systemd`-maintained,
//!   machine-unique id every NixOS host already has).
//! - User scope reads the hostname, because a user generation has no equivalent
//!   system-wide id and two accounts on the same host are expected to share
//!   one.

use xi_core::sources::Scope;

use crate::health::DegradedReason;

/// Where `/etc/machine-id` lives on a NixOS host.
pub const MACHINE_ID_PATH: &str = "/etc/machine-id";

/// Read this host's identifying token for `scope`.
///
/// System scope reads [`MACHINE_ID_PATH`]; user scope reads the hostname.
/// Both failure modes (file absent, hostname unreadable) collapse to
/// `None` rather than an error: a host with neither has nothing to guard
/// against a stale generation with, and the caller treats that as "cannot
/// decide", not as "matches".
#[must_use]
pub fn host_token(scope: Scope) -> Option<String> {
  host_token_with(scope, MACHINE_ID_PATH, &hostname)
}

/// [`host_token`] against an injectable machine-id path and hostname
/// source, for tests.
#[must_use]
pub fn host_token_with(
  scope: Scope,
  machine_id_path: &str,
  hostname_source: &dyn Fn() -> Option<String>,
) -> Option<String> {
  match scope {
    Scope::System => {
      std::fs::read_to_string(machine_id_path)
        .ok()
        .map(|raw| raw.trim().to_owned())
        .filter(|s| !s.is_empty())
    },
    Scope::User => hostname_source(),
  }
}

/// The real hostname source.
///
/// Delegates to [`xi_core::util::get_hostname`] rather than calling
/// `nix::unistd::gethostname` directly: that function already carries the
/// macOS special case (`SCDynamicStoreCopyLocalHostName` instead of the
/// POSIX call, which on macOS can return `localhost`), and a second,
/// POSIX-only hostname lookup here would silently disagree with it on
/// exactly the platform where it matters.
fn hostname() -> Option<String> {
  xi_core::util::get_hostname(None).ok()
}

/// Refuse activation when the generation's embedded id does not match
/// the host's.
///
/// `host` and `generation` are compared as opaque strings: the spec does
/// not case-fold or trim beyond what [`host_token`] already does, because
/// a `/etc/machine-id` mismatch that differs only by whitespace is still
/// evidence the generation was built for a different host, not for this
/// one with a formatting quirk.
///
/// A missing host token refuses rather than passing permissively: it is
/// the "cannot decide" case, and the guard exists precisely so a decision
/// the daemon cannot make is never silently treated as a match.
///
/// # Errors
///
/// Returns [`DegradedReason::MachineIdMismatch`] naming both ids.
pub fn guard(
  host: Option<&str>,
  generation: &str,
) -> Result<(), DegradedReason> {
  match host {
    Some(host) if host == generation => Ok(()),
    Some(host) => {
      Err(DegradedReason::MachineIdMismatch {
        host:       host.to_owned(),
        generation: generation.to_owned(),
      })
    },
    None => {
      Err(DegradedReason::MachineIdMismatch {
        host:       "<unknown>".to_owned(),
        generation: generation.to_owned(),
      })
    },
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use tempfile::NamedTempFile;

  use super::*;

  #[test]
  fn system_scope_reads_the_trimmed_machine_id_file() {
    let file = NamedTempFile::new().expect("tempfile");
    std::fs::write(file.path(), "deadbeefcafef00d\n").expect("write");
    let token = host_token_with(
      Scope::System,
      file.path().to_str().expect("utf8 path"),
      &|| None,
    );
    assert_eq!(token.as_deref(), Some("deadbeefcafef00d"));
  }

  #[test]
  fn system_scope_with_no_machine_id_file_is_undecidable() {
    let token =
      host_token_with(Scope::System, "/does/not/exist/machine-id", &|| None);
    assert_eq!(token, None);
  }

  #[test]
  fn user_scope_reads_the_injected_hostname() {
    let token = host_token_with(Scope::User, "/does/not/matter", &|| {
      Some("laptop.local".to_owned())
    });
    assert_eq!(token.as_deref(), Some("laptop.local"));
  }

  #[test]
  fn matching_ids_pass_the_guard() {
    assert!(guard(Some("AAAA"), "AAAA").is_ok());
  }

  /// SC-G-007 / the spec's "Mismatched machine-id refuses" scenario: both
  /// ids must be visible in the error so an operator can tell what is
  /// wrong without re-running the check by hand.
  #[test]
  fn mismatched_ids_refuse_and_name_both_values() {
    let err =
      guard(Some("AAAA"), "BBBB").expect_err("mismatch must be refused");
    assert_eq!(err.token(), "machine-id-mismatch");
    let text = err.to_string();
    assert!(text.contains("AAAA"), "got: {text}");
    assert!(text.contains("BBBB"), "got: {text}");
  }

  #[test]
  fn an_undecidable_host_token_refuses_rather_than_passing() {
    let err =
      guard(None, "BBBB").expect_err("an unknown host id must not pass");
    assert_eq!(err.token(), "machine-id-mismatch");
  }
}
