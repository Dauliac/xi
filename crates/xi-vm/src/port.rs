//! Runtime host-port resolution for `xi vm start`.
//!
//! Per `openspec/changes/xi-vm-lifecycle/design.md`: port availability is a
//! property of the host at launch time, not something Nix eval can see, so
//! `vm-guest.nix` bakes no `forwardPorts` and every port used by a running
//! instance is resolved here instead.

use std::net::TcpListener;

/// First port tried for the implicit SSH forward, before incrementing on
/// collision. Arbitrary but high enough to rarely collide with anything a
/// developer runs on purpose.
pub const SSH_BASE_PORT: u16 = 22220;

/// How many consecutive ports to try before giving up.
const MAX_PROBE_ATTEMPTS: u16 = 200;

/// Whether `port` is free to bind on `127.0.0.1` right now.
///
/// A TCP-bind probe, not a guarantee: the port can be taken by another
/// process between this check and the QEMU invocation actually binding it
/// (the same race any "find a free port" scheme has). `xi vm start` relies
/// on QEMU's own bind failure as the final word, this is just to avoid the
/// common case of handing QEMU an obviously-taken port.
#[must_use]
pub fn is_free(port: u16) -> bool {
  TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Probe upward from `base` for the first free port.
///
/// # Errors
///
/// Returns an error naming the exhausted range if nothing in
/// `[base, base + MAX_PROBE_ATTEMPTS)` is free.
pub fn resolve_free_port(base: u16) -> color_eyre::Result<u16> {
  for offset in 0..MAX_PROBE_ATTEMPTS {
    let candidate = base.saturating_add(offset);
    if is_free(candidate) {
      return Ok(candidate);
    }
    if candidate == u16::MAX {
      break;
    }
  }
  color_eyre::eyre::bail!(
    "no free port found in {base}..{} after {MAX_PROBE_ATTEMPTS} attempts",
    base.saturating_add(MAX_PROBE_ATTEMPTS)
  );
}

/// Resolve the host port for one declared app forward.
///
/// Tries `hint` first when given; on collision, fails naming the conflict
/// unless `allow_fallback` is set, in which case it resolves dynamically
/// from `fallback_base` instead.
///
/// # Errors
///
/// Returns an error naming the conflicting port when `hint` is taken and
/// `allow_fallback` is false, or when dynamic resolution itself is
/// exhausted.
pub fn resolve_app_port(
  hint: Option<u16>,
  allow_fallback: bool,
  fallback_base: u16,
) -> color_eyre::Result<u16> {
  match hint {
    Some(port) if is_free(port) => Ok(port),
    Some(_port) if allow_fallback => resolve_free_port(fallback_base),
    Some(port) => {
      color_eyre::eyre::bail!(
        "host port {port} is already in use; pass --allow-port-fallback to \
         pick a different port automatically"
      );
    },
    None => resolve_free_port(fallback_base),
  }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use super::*;

  #[test]
  fn a_free_port_is_reported_free() {
    // Bind to port 0 to let the OS hand back a genuinely free one, then
    // drop the listener and confirm the probe agrees it's free again.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    assert!(is_free(port));
  }

  #[test]
  fn a_bound_port_is_reported_taken() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().expect("local_addr").port();
    assert!(!is_free(port));
  }

  #[test]
  fn resolve_free_port_skips_a_taken_base() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let taken = listener.local_addr().expect("local_addr").port();
    let resolved = resolve_free_port(taken).expect("resolve");
    assert_ne!(resolved, taken);
    assert!(is_free(resolved));
  }

  #[test]
  fn resolve_app_port_prefers_the_hint_when_free() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let free_port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    let resolved = resolve_app_port(Some(free_port), false, SSH_BASE_PORT)
      .expect("resolve");
    assert_eq!(resolved, free_port);
  }

  #[test]
  fn resolve_app_port_fails_loudly_on_a_taken_hint_without_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let taken = listener.local_addr().expect("local_addr").port();
    let err = resolve_app_port(Some(taken), false, SSH_BASE_PORT)
      .expect_err("must fail without --allow-port-fallback");
    assert!(err.to_string().contains(&taken.to_string()));
  }

  #[test]
  fn resolve_app_port_falls_back_on_a_taken_hint_when_allowed() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let taken = listener.local_addr().expect("local_addr").port();
    let resolved = resolve_app_port(Some(taken), true, SSH_BASE_PORT)
      .expect("resolve with fallback");
    assert_ne!(resolved, taken);
  }

  #[test]
  fn resolve_app_port_with_no_hint_resolves_dynamically() {
    let resolved =
      resolve_app_port(None, false, SSH_BASE_PORT).expect("resolve");
    assert!(is_free(resolved) || resolved >= SSH_BASE_PORT);
  }
}
