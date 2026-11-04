//! Why a tick stopped, in the vocabulary `xi sources status` prints.
//!
//! The spec pins four reason tokens verbatim  -  `remote-force-push`,
//! `signature-missing`, `missing-sentinel`, `hook-on_switch-failed`  -
//! because they are what an operator greps for and what a monitoring
//! rule matches on. They are defined here, once, and
//! [`DegradedReason::token`] is the only thing allowed to spell them.
//!
//! The `hook-on_switch-failed` token keeps the spec's underscore even
//! though the config key is `on-switch`: the token names the hook point,
//! not the config key, and the spec fixed its spelling.

use xi_core::sources::{GitopsHealth, Sha, conflict::PostAdvance};

/// Why the daemon could not complete a tick, or completed it degraded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DegradedReason {
  /// The sources dir has no `.xi-managed` sentinel.
  #[error("missing-sentinel: {detail}")]
  MissingSentinel {
    /// What exactly was missing or mismatched.
    detail: String,
  },

  /// The remote could not be reached or the fetch failed.
  #[error("fetch-failed: {remote}: {detail}")]
  FetchFailed {
    /// Which remote failed, so a multi-remote log is readable.
    remote: String,
    /// Git's own message.
    detail: String,
  },

  /// The remote tip is not a descendant of the recorded `sources_head`.
  #[error(
    "remote-force-push: rejected {rejected} (not a descendant of {held})"
  )]
  RemoteForcePush {
    /// The sha the daemon refused to move to.
    rejected: Sha,
    /// The sha the sources dir stays on.
    held:     Sha,
  },

  /// The tip carries no signature and `require_signed` is set.
  #[error("signature-missing: {sha} is unsigned")]
  SignatureMissing {
    /// The unsigned tip.
    sha: Sha,
  },

  /// The tip is signed but does not verify against any trust root.
  #[error("signature-invalid: {sha}: {detail}")]
  SignatureInvalid {
    /// The tip that failed verification.
    sha:    Sha,
    /// What the verifier said.
    detail: String,
  },

  /// The conflict policy refused the advance.
  #[error("{reason}")]
  ConflictRefused {
    /// The conflict capability's own reason token.
    reason: String,
  },

  /// A lifecycle hook exited non-zero.
  #[error("hook-{hook}-failed: exit {code}")]
  HookFailed {
    /// Hook point name, in the token's spelling (`on_switch`).
    hook: String,
    /// The hook's exit code, or -1 when it was signalled.
    code: i32,
  },

  /// The generation's embedded machine id does not match the host's.
  #[error("machine-id-mismatch: host {host}, generation {generation}")]
  MachineIdMismatch {
    /// The host's `/etc/machine-id` or hostname.
    host:       String,
    /// What the generation embeds.
    generation: String,
  },

  /// Registering a generation on the named profile failed.
  #[error("profile-register-failed: {detail}")]
  ProfileRegisterFailed {
    /// What `nix-env` said.
    detail: String,
  },

  /// Exec'ing `xi <scope> switch` (or the build-only probe run ahead of
  /// it) failed to spawn or exited non-zero.
  ///
  /// This is deliberately one token for both the probe and the real
  /// switch: an operator grepping for why a generation never landed does
  /// not need to know which of the two invocations failed, only that the
  /// delegated build/activate pipeline did not complete successfully.
  #[error("activation-failed: {detail}")]
  ActivationFailed {
    /// `xi`'s own stderr (or stdout, if stderr was empty).
    detail: String,
  },

  /// A write was aimed outside the sources dir.
  #[error("write-outside-sources-dir: {path}")]
  WriteOutsideSourcesDir {
    /// The path that was refused.
    path: String,
  },
}

impl DegradedReason {
  /// The stable token a monitoring rule matches on.
  ///
  /// Deliberately free of shas and paths: those belong in the message
  /// body, and an alert keyed on a token that changes every tick is not
  /// an alert.
  #[must_use]
  pub fn token(&self) -> String {
    match *self {
      Self::MissingSentinel { .. } => "missing-sentinel".to_owned(),
      Self::FetchFailed { .. } => "fetch-failed".to_owned(),
      Self::RemoteForcePush { .. } => "remote-force-push".to_owned(),
      Self::SignatureMissing { .. } => "signature-missing".to_owned(),
      Self::SignatureInvalid { .. } => "signature-invalid".to_owned(),
      Self::ConflictRefused { ref reason } => reason.clone(),
      Self::HookFailed { ref hook, .. } => format!("hook-{hook}-failed"),
      Self::MachineIdMismatch { .. } => "machine-id-mismatch".to_owned(),
      Self::ProfileRegisterFailed { .. } => {
        "profile-register-failed".to_owned()
      },
      Self::ActivationFailed { .. } => "activation-failed".to_owned(),
      Self::WriteOutsideSourcesDir { .. } => {
        "write-outside-sources-dir".to_owned()
      },
    }
  }

  /// The [`GitopsHealth`] the model folds into `xi sources status`.
  ///
  /// The reason carries the token first so the status line reads
  /// `Degraded, reason: remote-force-push (...)` even after the model
  /// prefixes it with its own "last gitops tick failed" wording.
  #[must_use]
  pub fn health(&self) -> GitopsHealth {
    GitopsHealth::Failed {
      reason: self.to_string(),
    }
  }
}

/// What one tick did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickOutcome {
  /// The remote had not moved. The common case.
  NoChange {
    /// The sha both ends are on.
    sha: Sha,
  },
  /// `sources_head` moved forward.
  Advanced {
    /// Where it was.
    from:         Option<Sha>,
    /// Where it is now.
    to:           Sha,
    /// A hook that failed after the advance landed. The advance stands:
    /// a hook is a notification, not a precondition.
    hook:         Option<DegradedReason>,
    /// What `sync_mode` says should happen to the working dir now. See
    /// [`xi_core::sources::conflict::post_advance`] for the mapping; the
    /// daemon's job is only to report this, not to run `QueueSync` or
    /// `Announce` itself - those are the next interactive command's and
    /// `xi sources status`'s jobs respectively.
    post_advance: PostAdvance,
  },
  /// The advance landed but activation waits for the reboot window.
  Deferred {
    /// The sha the sources dir is on.
    sha:     Sha,
    /// Minutes until the window opens.
    minutes: u32,
  },
  /// The tick stopped without changing anything.
  Degraded(
    /// Why.
    DegradedReason,
  ),
}

impl TickOutcome {
  /// The health this outcome implies.
  #[must_use]
  pub fn health(&self) -> GitopsHealth {
    match *self {
      Self::NoChange { .. } | Self::Deferred { .. } => GitopsHealth::Ok,
      Self::Advanced { ref hook, .. } => {
        hook
          .as_ref()
          .map_or(GitopsHealth::Ok, DegradedReason::health)
      },
      Self::Degraded(ref reason) => reason.health(),
    }
  }

  /// Whether this outcome left the sources dir on a new sha.
  #[must_use]
  pub const fn advanced(&self) -> bool {
    matches!(*self, Self::Advanced { .. })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tokens_match_the_spellings_the_spec_pins() {
    let force_push = DegradedReason::RemoteForcePush {
      rejected: "cafe".to_owned(),
      held:     "beef".to_owned(),
    };
    assert_eq!(force_push.token(), "remote-force-push");

    let unsigned = DegradedReason::SignatureMissing {
      sha: "cafe".to_owned(),
    };
    assert_eq!(unsigned.token(), "signature-missing");

    let sentinel = DegradedReason::MissingSentinel {
      detail: "no .xi-managed".to_owned(),
    };
    assert_eq!(sentinel.token(), "missing-sentinel");

    let hook = DegradedReason::HookFailed {
      hook: "on_switch".to_owned(),
      code: 1,
    };
    assert_eq!(hook.token(), "hook-on_switch-failed");
  }

  /// The force-push message must name the sha an operator has to go
  /// look at; the spec asks for it to be logged.
  #[test]
  fn force_push_message_names_both_shas() {
    let reason = DegradedReason::RemoteForcePush {
      rejected: "deadbeef".to_owned(),
      held:     "c0ffee".to_owned(),
    };
    let text = reason.to_string();
    assert!(text.contains("deadbeef"), "got: {text}");
    assert!(text.contains("c0ffee"), "got: {text}");
  }

  #[test]
  fn an_advance_with_a_failed_hook_is_still_an_advance() {
    let outcome = TickOutcome::Advanced {
      from:         None,
      to:           "cafe".to_owned(),
      hook:         Some(DegradedReason::HookFailed {
        hook: "on_switch".to_owned(),
        code: 3,
      }),
      post_advance: PostAdvance::Announce,
    };
    assert!(outcome.advanced());
    assert!(matches!(outcome.health(), GitopsHealth::Failed { .. }));
  }

  #[test]
  fn a_clean_no_change_tick_is_healthy() {
    let outcome = TickOutcome::NoChange {
      sha: "cafe".to_owned(),
    };
    assert_eq!(outcome.health(), GitopsHealth::Ok);
  }
}
