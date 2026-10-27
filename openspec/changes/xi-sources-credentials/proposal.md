# Change: xi-sources-credentials

## Why

Every xi-sources operation that touches a remote (fetch by the gitops daemon in
[[xi-sources-gitops]], clone during [[xi-sources-bootstrap]], rsync during
[[xi-sources-push]] `--mode sources`) assumes git auth "just works". It does
not. Users will configure private remotes with one of four common patterns, and
each has different ergonomics, failure modes, and security properties:

- SSH keys loaded in `ssh-agent` (interactive workstation)
- SSH deploy keys unique to one machine (server, laptop)
- HTTPS personal access tokens via `credential.helper`
- OAuth tokens from CI systems via `GIT_ASKPASS`

A spec that papers over this makes the daemon silently hang on the first fetch
of a private repo. This change gives xi an explicit surface for each pattern, a
fallthrough chain, and a documented failure mode when credentials are missing.

## What Changes

- Introduce capability `xi-sources-credentials` that owns:
  - A **four-mode credentials config** under
    `.xi.toml [sources.credentials.<kind>]` and the matching Nix module tree
    `xi.sources.credentials.<kind>.*`:
    - `[sources.credentials.ssh]` with `identity-file` (path), `use-agent`
      (bool, default true), `known-hosts-file` (path, optional override),
      `strict-host-key-checking` (enum `yes | no | accept-new`, default `yes`
      for user scope, `accept-new` for bootstrap first-run only).
    - `[sources.credentials.https]` with `token-file` (path to a file containing
      the PAT on a single line), `credential-helper` (str, alternative to
      `token-file`), and `username` (str, typically `git` for PAT auth).
    - `[sources.credentials.oauth]` with `askpass` (path to a script that prints
      the token; mirrors git's `GIT_ASKPASS`).
    - `[sources.credentials.anonymous]` with `allow` (bool, default true) to opt
      out of the fallthrough for repos that MUST be private.
  - A **fallthrough chain** evaluated per-fetch: xi tries SSH when the remote
    URL scheme is `ssh://` or `git@`, HTTPS when scheme is `https://`, and
    refuses when scheme is unsupported. Within each scheme, xi tries configured
    credential modes in order (`identity-file`, then `use-agent`, then
    anonymous) and reports the first failure that was NOT a credential issue (so
    a wrong identity does not mask an unreachable host).
  - A **secret-at-rest guard**: the capability MUST read token files and
    identity files via their path only; the token value MUST NOT land in
    `xi sources status`, logs, or any closure. The path's contents are read into
    memory only for the duration of a single git subprocess.
  - A **sops-nix / agenix integration pattern** (not a reinvention):
    `token-file` and `identity-file` accept paths that are managed by sops-nix
    or agenix. xi does not decrypt; it reads the plaintext path after the secret
    manager has mounted it. The documentation names the pattern; xi code treats
    it like any other path.
  - A **host-key trust-on-first-use (TOFU)** policy for the bootstrap flow only:
    when `credentials.ssh.strict-host-key-checking = "accept-new"` the FIRST
    fetch may add the host key to `known_hosts`; subsequent fetches with the
    same host but a different key MUST refuse. Any non-bootstrap fetch MUST
    default to `yes` (reject unknown hosts).

## Impact

- **Affected specs**: `xi-sources-credentials` (new capability).
- **Affected code (planned)**: `crates/xi-sources/src/credentials.rs` owning the
  fallthrough chain; integration points in the git subprocess invocations across
  daemon, bootstrap, and push.
- **Coordination**: [[xi-sources-gitops]] (daemon fetches honour this),
  [[xi-sources-bootstrap]] (phase 3 clone honours this, with the TOFU
  exception), [[xi-sources-push]] (`--mode sources` rsync honours this when the
  target repo is remote), [[xi-sources-nix-modules]] (option tree extension).
- **Breaking?**: No. Users with public repos see no change. Users with private
  repos were silently failing before; they now configure the surface explicitly.
- **Migration**: None.

## Success Criteria

- **SC-R-001**: With no credentials configured and a public remote, fetches
  succeed anonymously (SSH anonymous or HTTPS unauthenticated).
- **SC-R-002**: With no credentials configured and a private remote, the
  daemon's first tick reports `status: Degraded, reason:
  credentials-missing`
  naming the remote URL and the credentials config keys that would unblock it.
  No busy-loop.
- **SC-R-003**: `[sources.credentials.ssh] identity-file = "..."` passes the
  identity to git via `GIT_SSH_COMMAND='ssh -i <path>'`; the daemon does not
  touch `~/.ssh/config`.
- **SC-R-004**: `[sources.credentials.https] token-file = "..."` passes the
  token via a one-shot `credential.helper` that reads the file and emits
  `password=<content>`; the token never appears in any xi log or envelope.
- **SC-R-005**: TOFU during bootstrap adds the host key to `known_hosts` on
  first fetch; the matching line is logged with the key fingerprint; a
  subsequent fetch with a different key refuses.
- **SC-R-006**: `credentials.anonymous.allow = false` with a public remote still
  refuses anonymous fetch; the user must configure one of the three
  authenticated modes.
- **SC-R-007**: A token file managed by sops-nix is read correctly after the
  secret manager has mounted it; a dangling path (sops not yet mounted) reports
  `Pending` with a message naming the mount path.
- **SC-R-008**: `xi sources status` MUST NOT include any token or key value; the
  response includes only the credential-mode name and the path reference.
