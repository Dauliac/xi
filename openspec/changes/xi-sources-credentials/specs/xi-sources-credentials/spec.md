# Capability: xi-sources-credentials

## ADDED Requirements

### Requirement: Four-mode credentials config

The capability MUST expose exactly four credential kinds under
`.xi.toml [sources.credentials.<kind>]` and the matching Nix module tree
`xi.sources.credentials.<kind>.*`:

- `ssh`: `identity-file` (path, nullable), `use-agent` (bool, default true),
  `known-hosts-file` (path, nullable), `strict-host-key-checking` (enum
  `yes | no | accept-new`, default `yes` outside bootstrap).
- `https`: `token-file` (path, nullable), `credential-helper` (str, nullable),
  `username` (str, default `"git"`).
- `oauth`: `askpass` (path to an executable, nullable).
- `anonymous`: `allow` (bool, default true).

A kind with no fields set means "mode disabled". Setting any field in a kind
MUST enable it for the fallthrough chain.

#### Scenario: Unknown credential kind refused

- **GIVEN** `.xi.toml` contains `[sources.credentials.frobnicate]`
- **WHEN** the config loader reads the file
- **THEN** the command exits non-zero with a message naming the unknown kind

### Requirement: Fallthrough chain

Every git subprocess spawned by xi (fetch, clone, push, rsync in
`--mode sources`) MUST route credentials via a fixed fallthrough:

1. Parse the remote URL. Scheme `ssh://` and shortcut `git@` route to the `ssh`
   kind. Scheme `https://` routes to the `https` kind. Other schemes refuse with
   `status: Degraded, reason:
   unsupported-scheme`.
2. Within the routed kind, try modes in order:
   - `ssh`: identity-file, then agent, then anonymous (if allowed).
   - `https`: token-file, then credential-helper, then anonymous (if allowed).
   - `oauth`: `askpass` only; no fallthrough.
3. If every mode errors with a credential-shaped failure (git exit code
   pattern), report `status: Degraded, reason:
   credentials-missing` naming
   the remote URL and the config keys that would unblock it. If any mode errors
   with a non-credential failure (DNS, TCP, HTTP 5xx), report that error
   instead.

#### Scenario: Non-credential error wins over credential error

- **GIVEN** DNS fails for `host.example`
- **WHEN** fetch runs
- **THEN** the error is `dns-failure`, not `credentials-missing`, even if
  credentials are also unconfigured

### Requirement: SSH `GIT_SSH_COMMAND` composition

When routing to the ssh kind with `identity-file` set, the capability MUST
compose `GIT_SSH_COMMAND` as
`ssh -i <identity-file> -o IdentitiesOnly=yes
-o UserKnownHostsFile=<known-hosts-file>
-o StrictHostKeyChecking=<policy>`.

`IdentitiesOnly=yes` is mandatory to prevent ssh-agent from offering other keys.
The identity-file MUST be a path readable by the invoking process (daemon user
for daemon, operator for one-shot invocations).

#### Scenario: Agent not offered when identity-file set

- **GIVEN** `credentials.ssh.identity-file = "/run/secrets/deploy_key"` AND an
  agent socket is live with ten other identities
- **WHEN** fetch runs
- **THEN** ssh is invoked with `IdentitiesOnly=yes` and only the deploy key is
  offered

### Requirement: HTTPS one-shot credential helper

When routing to the https kind with `token-file` set, the capability MUST act as
a one-shot `credential.helper` that:

1. Reads the file at `token-file` into memory.
2. On git's `credential helper fill` protocol input on stdin, responds with
   `username=<configured username>\npassword=<token>\n`.
3. Zeroes the in-memory token buffer before process exit.

The token's value MUST NOT appear in any xi log, response envelope, or
environment variable.

#### Scenario: Token redacted everywhere

- **GIVEN** a successful fetch using a token
- **WHEN** the operator greps the daemon log and `xi sources status
  --explain`
  output for the token string
- **THEN** both searches return zero matches

### Requirement: TOFU is bootstrap-only

The `strict-host-key-checking = "accept-new"` policy is permitted ONLY during
the bootstrap flow ([[xi-sources-bootstrap]] phase 3). On first use the host key
is added to the configured known-hosts file and the fingerprint is logged. All
subsequent fetches on that host MUST use `yes` (reject unknown) regardless of
config, so a man-in-the-middle swap of the host key is caught.

Any non-bootstrap fetch with `strict-host-key-checking = "accept-new"` MUST be
silently promoted to `yes` and logged.

#### Scenario: TOFU accepts first key, refuses swap

- **GIVEN** bootstrap of a fresh machine against `git.example`
- **WHEN** phase 3 clone runs with `accept-new`
- **THEN** the host key is added to known-hosts and the fingerprint is logged
- **AND** a later fetch with a different host key from the same host refuses
  with `status: Degraded, reason: host-key-changed`

### Requirement: Opaque credential type

The config loader MUST expose each credential value through an opaque newtype
whose `Debug` implementation prints `<redacted>`, whose `Serialize`
implementation refuses, and whose only accessor is a controlled
`with_secret<F>(f: F)` closure. The closure's return value is reinspected by a
lint rule to prevent accidental leakage.

#### Scenario: Debug prints redacted

- **GIVEN** a loaded credential config
- **WHEN** debug-formatting the config struct
- **THEN** every credential field prints `<redacted>` without the underlying
  value
