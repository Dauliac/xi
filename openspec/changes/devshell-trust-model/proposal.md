# Change: devshell-trust-model

## Why

`xi develop` runs user-authored Nix code (`flake.nix`, imports, IFDs) whenever
the shell enters a flake directory. Automatic evaluation of unverified code on
every `cd` is unsafe: cloning a hostile repo, then walking into it, must not
implicitly launch a Nix evaluation with the ambient user environment.

The current implementation (`crates/xi-develop/src/trust.rs`) uses a marker file
per canonical flake id to gate automatic activation, but the _contract_ behind
that mechanism has never been specified: what a "trusted" flake grants, when the
trust decision is made, how it interacts with the prompt hook, and what a
_non-trusted_ flake's UX looks like. Without a spec, safe defaults drift each
time the code is touched.

## What Changes

- Establish a formal capability `xi-devshell-trust` that owns:
  - The per-flake trust marker (single, boolean, keyed by `dirs::flake_id` of
    the canonicalised flake root).
  - The **explicit** grant/revoke lifecycle (`xi develop trust`,
    `xi develop untrust`).
  - The **refuse-by-default** behaviour of the prompt hook when a flake is
    untrusted.
  - The interactive-stderr **user-actionable prompt** carve-out (untrusted trust
    prompt is the _only_ prompt-time interactive stderr write the client is
    allowed to make — every other diagnostic goes to `hook.log`; the daemon/hook
    budget requirements in `xi-devshell-daemon` explicitly reserve stderr for
    this case).
- Mandate that untrusted flakes MUST NOT trigger daemon spawn or Nix evaluation
  at prompt time.
- Mandate that trust decisions are keyed by the **canonical** flake root path
  and survive `flake.nix` edits — modifying trusted code does not require
  re-trusting, but moving/renaming the directory does (canonicalisation changes
  the id).
- Mandate that trust markers are readable only by the owning UNIX user (mode
  `0700` on the directory, `0600` on the marker) — trust is not shareable across
  users on the same host.
- Add a subcommand `xi develop trust --list` returning every currently-trusted
  flake id with its canonical path (best-effort, since paths can move).

## Impact

- **Affected specs**: `xi-devshell-trust` (new capability — first delta
  introduces its requirements).
- **Affected code**: `crates/xi-develop/src/trust.rs`,
  `crates/xi-develop/src/dirs.rs` (flake id derivation, trust dir path), prompt
  hook entry (`crates/xi-develop/src/hook/tick.rs` and
  `crates/xi-develop/src/prompt.rs` — refuse-by-default guard),
  `crates/xi-develop/src/args.rs` (add `trust --list`).
- **Coordination with `async-daemon-cqrs`**: the client-side stderr-discipline
  requirement in `xi-devshell-daemon` explicitly names "user-actionable trust
  prompts" as the sole exception. This change formalises that carve-out on the
  trust side; the two specs must be read together.
- **Breaking?**: No. Existing marker layout is preserved; the change is
  specification of behaviour that is already the intent of the current code.
  `trust --list` is additive.
- **Migration**: None. Existing markers under `dirs::trust_dir()` remain valid.

## Success Criteria

- **SC-T-001**: Walking into an untrusted flake with the prompt hook installed
  does NOT spawn a daemon and does NOT run `nix eval` at prompt time.
- **SC-T-002**: `xi develop trust <path>` writes exactly one marker file at
  `dirs::trust_dir()/<flake_id>` with permissions `0600` and the containing
  directory `0700`.
- **SC-T-003**: `xi develop untrust <path>` removes the marker; a subsequent
  prompt in that flake reverts to the untrusted UX (single actionable prompt, no
  eval).
- **SC-T-004**: Editing `flake.nix` in a trusted flake does NOT invalidate the
  trust marker; the next prompt continues to activate.
- **SC-T-005**: Moving a trusted flake to a new canonical path (via `mv` or
  symlink change) requires a fresh `xi develop trust`; the marker under the old
  id is left intact but no longer matches the new location.
- **SC-T-006**: `xi develop trust --list` returns one line per marker with
  best-effort canonical path resolution; markers whose original path has
  disappeared are reported with `(path missing)`.
- **SC-T-007**: The trust prompt for an untrusted flake is the _only_
  interactive-stderr write the prompt hook produces; simulated five-minute
  daemon outage + walking through the untrusted flake produces exactly one
  stderr line (the trust prompt), not per-tick.

## Assumptions

- Trust is a per-user, per-flake grant; sharing trust across users on the same
  machine is out of scope (would need signed-marker + verification, deferred).
- Trust is a boolean, not a scoped capability (no "trust for `default` but not
  `ci`" in v1); scoping is a documented future extension.
- The prompt hook has already been installed by the user (this spec does not
  cover the installation UX; that lives in `xi-devshell-activation`).
- `dirs::flake_id` is a stable function of the canonical path bytes; changing
  its derivation is a breaking change to the trust store and requires a
  migration proposal of its own.
