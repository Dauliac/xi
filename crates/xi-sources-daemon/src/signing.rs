//! Signed-commit verification: `require_signed = true`.
//!
//! Only the tip commit is inspected, per spec and per comin's
//! `docs/howtos.md` guidance: verifying the whole history on every tick
//! would make a poll loop's cost grow with the repo's age for no benefit
//! - a tip that verifies came from someone holding a trusted key *now*, which
//!   is the property this gate exists to establish.
//!
//! Two trust roots, both optional, at least one required by
//! [`xi_core::sources::config::ResolvedSources::check_consistency`]
//! before this module is ever reached:
//!
//! - **SSH** (`allowed_signers`): git's native `gpg.format = ssh`, checked
//!   against an `allowed_signers` file in the `ssh-keygen -Y verify` format.
//! - **GPG** (`gpg_public_keys`): git's default `gpg.format = openpgp`, checked
//!   against a public keyring built fresh in a scratch `GNUPGHOME` for each
//!   verification, so the daemon's trust root is exactly the configured file
//!   and never the operator's own keyring.
//!
//! A tip that verifies against *either* configured root is accepted: a
//! repo may be SSH-signed, GPG-signed, or dual-signed, and the daemon
//! does not get to choose which one the operator meant to be primary.
//!
//! Every check goes through `git verify-commit`, not through
//! `log --format=%G?` alone: `%G?` reports whether the cryptographic
//! signature is well-formed, but for SSH signatures it does **not**
//! reflect whether the signer is actually a principal in the configured
//! `allowed_signers` file - `verify-commit`'s exit code is the only
//! place that check is enforced. `%G?` is still used, but only *after*
//! `verify-commit` has already failed, to tell "no signature at all"
//! (`N`) apart from "a signature that does not verify" for the
//! `Degraded` reason's token.
//!
//! Git is deliberately not behind the [`crate::exec::Exec`] seam here,
//! for the same reason [`crate::fetch`] keeps git direct: a real git
//! repo plus a real signing key in a tempdir is what the spec's
//! scenarios actually need to exercise.

use std::{
  path::Path,
  process::{Command, Output},
};

use tempfile::TempDir;

use crate::health::DegradedReason;

/// `%G?` letter for "no signature at all".
const NO_SIGNATURE: char = 'N';

/// How one trust root judged the tip.
enum RootCheck {
  /// `verify-commit` exited zero: a valid signature from a principal in
  /// the configured trust root.
  Valid,
  /// The tip carries no signature in any format `verify-commit` could
  /// find.
  NoSignature,
  /// The tip is signed, but not by a principal the configured root
  /// trusts (or `verify-commit` otherwise rejected it).
  Invalid {
    /// What `verify-commit` or the signature-letter probe said.
    detail: String,
  },
}

/// Verify the tip commit `sha` in `repo` against whichever trust roots
/// are configured.
///
/// `repo` must already have `sha` in its object database (i.e. this runs
/// after the fetch that brought it in).
///
/// # Errors
///
/// Returns [`DegradedReason::SignatureMissing`] when every configured
/// root reports no signature, and [`DegradedReason::SignatureInvalid`]
/// when at least one root saw a signature but none of them accepted it.
pub fn verify_tip(
  repo: &Path,
  sha: &str,
  allowed_signers: Option<&Path>,
  gpg_public_keys: Option<&Path>,
) -> Result<(), DegradedReason> {
  let mut saw_invalid: Option<String> = None;
  let mut saw_missing = false;

  for check in [
    allowed_signers.map(|file| check_ssh(repo, sha, file)),
    gpg_public_keys.map(|file| check_gpg(repo, sha, file)),
  ]
  .into_iter()
  .flatten()
  {
    match check {
      RootCheck::Valid => return Ok(()),
      RootCheck::NoSignature => saw_missing = true,
      RootCheck::Invalid { detail } => saw_invalid = Some(detail),
    }
  }

  match saw_invalid {
    Some(detail) => {
      Err(DegradedReason::SignatureInvalid {
        sha: sha.to_owned(),
        detail,
      })
    },
    None if saw_missing => {
      Err(DegradedReason::SignatureMissing {
        sha: sha.to_owned(),
      })
    },
    None => {
      Err(DegradedReason::SignatureMissing {
        sha: sha.to_owned(),
      })
    },
  }
}

/// Check `sha` against `allowed_signers` with git's `gpg.format = ssh`.
fn check_ssh(repo: &Path, sha: &str, allowed_signers: &Path) -> RootCheck {
  let config = [
    "-c".to_owned(),
    "gpg.format=ssh".to_owned(),
    "-c".to_owned(),
    format!("gpg.ssh.allowedSignersFile={}", allowed_signers.display()),
  ];
  classify(
    verify_commit(repo, sha, &config, &[]),
    signature_letter(repo, sha, &config, &[]),
  )
}

/// Check `sha` against a scratch keyring seeded from `gpg_public_keys`,
/// with git's default `gpg.format = openpgp`.
///
/// A fresh `GNUPGHOME` per call keeps the trust root exactly the
/// configured file: the operator's real keyring (which may trust all
/// sorts of keys for all sorts of other reasons) is never consulted.
fn check_gpg(repo: &Path, sha: &str, gpg_public_keys: &Path) -> RootCheck {
  let Some(scratch) = TempDir::new().ok() else {
    return RootCheck::Invalid {
      detail: "could not create a scratch GNUPGHOME".to_owned(),
    };
  };
  let import = Command::new("gpg")
    .env("GNUPGHOME", scratch.path())
    .args(["--batch", "--quiet", "--import"])
    .arg(gpg_public_keys)
    .output();
  if !matches!(&import, Ok(out) if out.status.success()) {
    return RootCheck::Invalid {
      detail: "could not import the configured gpg public key".to_owned(),
    };
  }

  let env = [("GNUPGHOME", scratch.path().to_string_lossy().into_owned())];
  classify(
    verify_commit(repo, sha, &[], &env),
    signature_letter(repo, sha, &[], &env),
  )
}

/// Turn a `verify-commit` result, plus (only computed when it failed) a
/// `%G?` letter, into one [`RootCheck`].
fn classify(
  verified: Result<(), String>,
  letter_if_needed: Option<char>,
) -> RootCheck {
  match verified {
    Ok(()) => RootCheck::Valid,
    Err(detail) => {
      match letter_if_needed {
        Some(NO_SIGNATURE) => RootCheck::NoSignature,
        _ => RootCheck::Invalid { detail },
      }
    },
  }
}

/// Run `git verify-commit <sha>` with `config` and `env` applied.
///
/// `Ok(())` on a zero exit; `Err(detail)` (git's stderr, or a generic
/// message when stderr was empty - `verify-commit` says nothing on an
/// unsigned commit) otherwise.
fn verify_commit(
  repo: &Path,
  sha: &str,
  config: &[String],
  env: &[(&str, String)],
) -> Result<(), String> {
  let mut command = Command::new("git");
  command.env("GIT_TERMINAL_PROMPT", "0");
  for (key, value) in env {
    command.env(key, value);
  }
  command.arg("-C").arg(repo);
  command.args(config);
  command.args(["verify-commit", sha]);

  let Ok(output) = command.output() else {
    return Err("could not run `git verify-commit`".to_owned());
  };
  if output.status.success() {
    Ok(())
  } else {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(if stderr.is_empty() {
      "verify-commit rejected the tip".to_owned()
    } else {
      stderr
    })
  }
}

/// The `%G?` letter for `sha`, under the same `config`/`env`.
///
/// Called only to classify a `verify-commit` failure as "no signature"
/// (`N`) versus everything else; see the module doc for why
/// `verify-commit`'s own exit code, not this letter, is the acceptance
/// check.
fn signature_letter(
  repo: &Path,
  sha: &str,
  config: &[String],
  env: &[(&str, String)],
) -> Option<char> {
  let mut command = Command::new("git");
  command.env("GIT_TERMINAL_PROMPT", "0");
  for (key, value) in env {
    command.env(key, value);
  }
  command.arg("-C").arg(repo);
  command.args(config);
  command.args(["log", "-1", "--format=%G?", sha]);
  let output = command.output().ok()?;
  letter_from_output(&output)
}

fn letter_from_output(output: &Output) -> Option<char> {
  String::from_utf8_lossy(&output.stdout)
    .trim()
    .chars()
    .next()
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "Fine in tests")]
mod tests {
  use std::fs;

  use tempfile::TempDir;

  use super::*;

  fn git(at: &Path, args: &[&str]) {
    let status = Command::new("git")
      .env("GIT_TERMINAL_PROMPT", "0")
      .arg("-C")
      .arg(at)
      .args(args)
      .status()
      .expect("git should run");
    assert!(status.success(), "git {args:?} failed");
  }

  /// A fresh ed25519 keypair plus an `allowed_signers` file naming its
  /// public half, for the SSH-signing path.
  struct SshIdentity {
    _home:           TempDir,
    private_key:     std::path::PathBuf,
    allowed_signers: std::path::PathBuf,
  }

  impl SshIdentity {
    fn generate() -> Self {
      let home = TempDir::new().expect("tempdir");
      let private_key = home.path().join("key");
      let status = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-C", "tester", "-f"])
        .arg(&private_key)
        .status()
        .expect("ssh-keygen should run");
      assert!(status.success(), "ssh-keygen failed");

      let public_key =
        fs::read_to_string(home.path().join("key.pub")).expect("pub key");
      let allowed_signers = home.path().join("allowed_signers");
      fs::write(
        &allowed_signers,
        format!("tester@example namespaces=\"git\" {public_key}"),
      )
      .expect("write allowed_signers");

      Self {
        _home: home,
        private_key,
        allowed_signers,
      }
    }
  }

  /// A git repo with one commit, optionally signed with `identity`.
  fn repo_with_commit(identity: Option<&SshIdentity>) -> (TempDir, String) {
    let tmp = TempDir::new().expect("tempdir");
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    git(tmp.path(), &["config", "user.email", "test@example.com"]);
    git(tmp.path(), &["config", "user.name", "Test"]);
    fs::write(tmp.path().join("file.txt"), "hi").expect("write");
    git(tmp.path(), &["add", "-A"]);

    if let Some(identity) = identity {
      git(tmp.path(), &["config", "gpg.format", "ssh"]);
      git(tmp.path(), &[
        "config",
        "user.signingkey",
        identity.private_key.to_str().expect("utf8 path"),
      ]);
      git(tmp.path(), &["commit", "-q", "-S", "-m", "signed"]);
    } else {
      git(tmp.path(), &[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "unsigned",
      ]);
    }

    let output = Command::new("git")
      .arg("-C")
      .arg(tmp.path())
      .args(["rev-parse", "HEAD"])
      .output()
      .expect("rev-parse");
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (tmp, sha)
  }

  /// SC-G-004 / "SSH-signed tip accepted".
  #[test]
  fn an_ssh_signed_tip_verifies_against_its_allowed_signers_file() {
    let identity = SshIdentity::generate();
    let (repo, sha) = repo_with_commit(Some(&identity));

    let result =
      verify_tip(repo.path(), &sha, Some(&identity.allowed_signers), None);
    assert!(result.is_ok(), "expected ok, got {result:?}");
  }

  /// SC-G-004's mirror / "Unsigned tip refused".
  #[test]
  fn an_unsigned_tip_is_refused_as_signature_missing() {
    let identity = SshIdentity::generate();
    let (repo, sha) = repo_with_commit(None);

    let err =
      verify_tip(repo.path(), &sha, Some(&identity.allowed_signers), None)
        .expect_err("an unsigned tip must not verify");
    assert_eq!(err.token(), "signature-missing");
    assert!(err.to_string().contains(&sha), "got: {err}");
  }

  /// A signature that does not match any entry in the `allowed_signers`
  /// file is a different failure from no signature at all, and the spec
  /// gives it a different token. `verify-commit`'s own exit code is what
  /// catches this - `%G?` alone reports the signature as merely
  /// "unknown validity" and would wrongly accept it.
  #[test]
  fn a_signature_from_an_untrusted_key_is_refused_as_signature_invalid() {
    let signer = SshIdentity::generate();
    let (repo, sha) = repo_with_commit(Some(&signer));

    // A *different* identity's allowed_signers file does not name the
    // key that actually signed the tip.
    let stranger = SshIdentity::generate();
    let err =
      verify_tip(repo.path(), &sha, Some(&stranger.allowed_signers), None)
        .expect_err("a signature from an unlisted key must not verify");
    assert_eq!(err.token(), "signature-invalid");
  }

  #[test]
  fn gpg_signed_tip_verifies_against_an_imported_public_key() {
    let home = TempDir::new().expect("tempdir");
    let status = Command::new("gpg")
      .env("GNUPGHOME", home.path())
      .args([
        "--batch",
        "--quiet",
        "--passphrase",
        "",
        "--quick-generate-key",
        "tester@example.com",
        "ed25519",
        "sign",
        "0",
      ])
      .status()
      .expect("gpg should run");
    assert!(status.success(), "gpg key generation failed");

    let export = Command::new("gpg")
      .env("GNUPGHOME", home.path())
      .args(["--batch", "--armor", "--export", "tester@example.com"])
      .output()
      .expect("gpg export should run");
    assert!(export.status.success(), "gpg export failed");
    let public_key_path = home.path().join("pub.asc");
    fs::write(&public_key_path, &export.stdout).expect("write pub key");

    let repo_dir = TempDir::new().expect("tempdir");
    git(repo_dir.path(), &["init", "-q", "-b", "main"]);
    git(repo_dir.path(), &[
      "config",
      "user.email",
      "tester@example.com",
    ]);
    git(repo_dir.path(), &["config", "user.name", "Test"]);
    git(repo_dir.path(), &[
      "config",
      "user.signingkey",
      "tester@example.com",
    ]);
    fs::write(repo_dir.path().join("file.txt"), "hi").expect("write");
    git(repo_dir.path(), &["add", "-A"]);

    let status = Command::new("git")
      .env("GNUPGHOME", home.path())
      .arg("-C")
      .arg(repo_dir.path())
      .args(["commit", "-q", "-S", "-m", "signed"])
      .status()
      .expect("git commit should run");
    assert!(status.success(), "signed commit failed");

    let output = Command::new("git")
      .arg("-C")
      .arg(repo_dir.path())
      .args(["rev-parse", "HEAD"])
      .output()
      .expect("rev-parse");
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();

    let result =
      verify_tip(repo_dir.path(), &sha, None, Some(&public_key_path));
    assert!(result.is_ok(), "expected ok, got {result:?}");
  }
}
