# Design: the sources entrypoint and CLI topology

## Status

Accepted. Supersedes the `xi sources init` surface introduced earlier in this
change and the top-level `xi sources` command group.

## Context

Three capabilities grew a user-facing surface independently and ended up
disagreeing about how an operator points xi at a config repo.

`xi-sources-bootstrap` specified the one-liner the whole effort exists for:

```
curl install.xi.dev | sh -s -- system switch https://git.example/dotfiles
```

The shell stub downloads the binary and `exec`s `xi system switch <remote-url>`.
Switch is the entrypoint, and installing Nix is a phase inside it.

`xi-sources-config` then added `xi sources init <remote-url>` as a second way
in: write the `[sources]` block, create the directories, clone. That is the same
job under a different name.

Separately, `xi sources status --scope {system,user}` put scope in a flag, even
though `xi-sources-model` already defines a resolver mapping each switch verb to
exactly one scope: `nixos`, `darwin` and `system` are System, `home` is User.

So the surface had two defects: two entrypoints for one job, and scope expressed
twice in two incompatible ways.

## Decision Drivers

- **One command to learn.** The same invocation must work on a bare machine and
  on a machine already running xi. An operator should not have to know which
  situation they are in before choosing a verb.
- **Scope must not be stateable twice.** If the verb already determines scope, a
  `--scope` flag can contradict it, and something has to lose.
- **`curl | sh` must stay the install story.** Whatever the shell stub `exec`s
  is by definition the entrypoint.
- **Reset, inspection and daemon control are real needs**, but they are operator
  maintenance, not the happy path.

## Considered Options

### Option 1: Keep `xi sources init`, make switch an alias

Switch would forward to init when handed a URL.

Rejected. It keeps two names for one job and leaves the question "which one is
canonical" permanently open. It also leaves `curl | sh` pointing at the
non-canonical one.

### Option 2: Keep both, document init as the "explicit" form

Rejected for the same reason, and it makes the bootstrap stub's
`exec xi system switch` a special case rather than the main path.

### Option 3: Switch is the only entrypoint; sources becomes maintenance

Accepted. See below.

## Decision

**`xi <scope> switch [<remote-url>]` is the single entrypoint**, where `<scope>`
is one of `nixos`, `darwin`, `system`, `home`.

- With a `<remote-url>`: adopt that remote as this scope's source, record it,
  clone it, then switch. This is the from-zero case and the change-my-remote
  case.
- Without one: resolve the remote through the existing precedence ladder and
  switch. This is the everyday case.

**Installing Nix is a pre-check inside switch**, not a separate command. Switch
probes for a usable `nix`; if absent and `installer` is not `none`, it delegates
to the configured installer backend before continuing. This is what lets the
bootstrap stub be nothing more than "download xi, exec switch": the binary
carries the whole flow.

**`sources` becomes a subcommand of each scope verb**, not a top-level command:

```
xi home   sources status
xi system sources status
xi nixos  sources status
xi darwin sources status
```

Scope therefore comes from the parent verb and the `--scope` flag is removed.
The `[sources] scope` config key is removed with it: it existed only to
disambiguate a flag that no longer exists.

The `sources` group keeps the operator-maintenance verbs (`status` today;
`sync`, `reset` and daemon control as their changes land). It is not the way a
user sets a machine up.

## Consequences

### Positive

- One entrypoint, and it is the one the install one-liner already uses.
- Scope is unrepresentable twice, so it cannot contradict itself. The
  verb-to-scope resolver in `xi-sources-model` becomes the only answer.
- `xi home switch` and `xi system switch` read as what they are: deploy this
  scope. The config repo is an argument, not a separate ceremony.
- The bootstrap stub needs no xi-specific knowledge beyond the verb.

### Negative

- `xi sources init` and `xi sources status --scope` ship and then change. Both
  are unreleased, so the blast radius is this repository.
- Four `sources` subcommand registrations instead of one. Mitigated by defining
  the group once and mounting it from each scope crate.
- `xi <scope> switch <arg>` now has a positional that can be either a flake
  installable or a config-repo remote. Switch has to tell them apart, and the
  rule has to be written down rather than guessed. See below.

## The discrimination rule

Requiring an `--adopt` flag was considered and rejected: it breaks the "same
command from zero or on an existing system" property that is the whole point of
this decision, and it would make the bootstrap one-liner carry a flag that
exists only to disambiguate an implementation detail.

The two argument shapes are distinguishable without a flag, because a git remote
URL is mostly not a valid bare flake reference:

| Positional                    | Read as            |
| ----------------------------- | ------------------ |
| absent                        | recorded source    |
| `git@host:owner/repo.git`     | config-repo remote |
| `https://host/owner/repo.git` | config-repo remote |
| `ssh://host/path`             | config-repo remote |
| `.`, `./cfg`, `/abs/path`     | flake installable  |
| `github:owner/repo`           | flake installable  |
| `git+https://host/repo`       | flake installable  |
| `path:...`, `/nix/store/...`  | flake installable  |

The rule: **scp-style `user@host:path`, and bare `https`/`http`/`ssh` URLs, are
config-repo remotes. Everything xi already parses as an installable stays an
installable.** `github:owner/repo` and the explicit `git+` schemes keep their
current meaning, so `xi system switch <local-flake>` and
`xi system switch github:you/cfg` behave exactly as they do today. That is what
honours the "Breaking?: No" promise in the bootstrap proposal.

`--adopt` and `--no-adopt` stay available to force either reading for an
argument that sits on the boundary, but neither is needed on the happy path and
the bootstrap stub passes neither.

Adoption is idempotent: adopting the remote already recorded for this scope
updates the clone and switches rather than erroring. That is what makes one
command correct on a machine that is already managed.

## Related

- [[xi-sources-model]] owns the verb-to-scope resolver this leans on.
- [[xi-sources-bootstrap]] owns the installer backends and the receipt.
- [[xi-sources-gitops]] owns what happens after adoption, on a timer.
