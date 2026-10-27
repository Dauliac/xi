# Tasks: xi-sources-credentials

Live state in beads under `change:xi-sources-credentials`.

## 1. Specification (P1)

- [ ] 1.1 Land `specs/xi-sources-credentials/spec.md`.
- [ ] 1.2 Cross-link with gitops, bootstrap, push, nix-modules.

## 2. Config surface (P1)

- [ ] 2.1 Add `[sources.credentials.ssh]`, `[sources.credentials.https]`,
      `[sources.credentials.oauth]`, `[sources.credentials.anonymous]` to the
      schema owned by [[xi-sources-config]].
- [ ] 2.2 Env mirrors: `XI_SOURCES_CREDENTIALS_SSH_IDENTITY_FILE`,
      `XI_SOURCES_CREDENTIALS_HTTPS_TOKEN_FILE`, etc.
- [ ] 2.3 Nix module parity under `xi.sources.credentials.*`
      ([[xi-sources-nix-modules]]).

## 3. Fallthrough chain (P1)

- [ ] 3.1 Scheme routing: `ssh://` and `git@`-shaped URLs route to SSH;
      `https://` routes to HTTPS; others refuse.
- [ ] 3.2 Per-scheme mode order: identity file, agent, anonymous. First
      non-credential error (network, DNS) wins over credential errors.

## 4. SSH integration (P1)

- [ ] 4.1 Build `GIT_SSH_COMMAND` with `-i <identity-file>` and
      `-o IdentitiesOnly=yes` to prevent agent fallthrough when the user wants a
      specific key.
- [ ] 4.2 Known-hosts file: default to the user's `~/.ssh/known_hosts` (user
      scope) or `/etc/ssh/ssh_known_hosts` (system scope); overridable via
      config.

## 5. HTTPS integration (P1)

- [ ] 5.1 One-shot `credential.helper` script embedded in the daemon binary.
      Receives `credential.helper fill` protocol on stdin, reads the token file,
      emits `password=<content>` on stdout.
- [ ] 5.2 Zero-copy: the token is read into memory, written to the git
      subprocess stdin, then zeroed.

## 6. TOFU for bootstrap (P1)

- [ ] 6.1 Only during the bootstrap flow and only when
      `strict-host-key-checking = "accept-new"`, pass that value to
      `GIT_SSH_COMMAND`. Log the accepted key fingerprint.
- [ ] 6.2 All other code paths default to `yes` (reject unknown).

## 7. Secret-at-rest guard (P1)

- [ ] 7.1 A static lint rule rejects any `tracing::` call that takes a
      credential value as a parameter. The config loader exposes credentials as
      an opaque newtype that redacts in `Debug` and refuses `Serialize`.
- [ ] 7.2 `xi sources status --explain` prints only the credential mode name and
      the configured path, never the file contents.

## 8. Tests (P1)

- [ ] 8.1 BDD: public remote, no credentials, success.
- [ ] 8.2 BDD: private remote, no credentials, `Degraded` `credentials-missing`.
- [ ] 8.3 BDD: SSH identity-file success.
- [ ] 8.4 BDD: HTTPS token-file success; grep logs for the token string returns
      zero matches.
- [ ] 8.5 BDD: TOFU accepts the first host key, refuses a second different key.

## 9. Follow-ups (P4)

- [ ] 9.1 Hardware-token auth (YubiKey ssh signing). Deferred.
- [ ] 9.2 Credential caching with TTL. Deferred.
- [ ] 9.3 Multi-identity routing (identity A for remote X, identity B for remote
      Y) when multi-remote lands. Deferred.
