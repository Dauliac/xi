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

## Verified Against Current Code (2026-11-07)

Re-read against `crates/xi-develop/src/trust.rs` and
`crates/xi-develop/src/prompt.rs`:

- **Not yet implemented** (tracked as open tasks in `tasks.md`; the "What
  Changes" bullets on permissions and prompt throttling below describe required,
  not current, behaviour): `trust_flake`/`untrust_flake` call plain
  `fs::write`/`fs::create_dir_all` (`trust.rs:27-29`) with no `set_permissions`
  call, so marker/directory permissions follow the process umask, not a forced
  `0600`/`0700`. And `handle_parent` in `prompt.rs:113-120` prints the "run
  `xi develop trust`" warning unconditionally on every tick a shell sits in an
  untrusted flake; there is no throttle key anywhere in the codebase.
- **Undocumented existing behaviour, specified below for the first time**:
  before any trust check, `prompt.rs::flake_likely_has_devshell`
  (`prompt.rs:434-441`) greps `flake.nix` for `devShells`/`devShell`/`mkShell`
  and skips the entire trust flow (no prompt, no daemon touch) when none are
  found.
- **Undocumented existing behaviour, specified below for the first time**:
  `prompt.rs::handle_subshell` "Case 3" (`prompt.rs:200-246`) independently
  trust-checks a nested flake encountered while already inside a trusted
  parent's subshell, spawns a nested subshell only if the child is also trusted,
  and otherwise silently stays in the parent with a path-keyed suppress flag (no
  escalation prompt).
- `xi develop trust --list` is confirmed absent from `args.rs`'s
  `DevelopAction`/`TargetArgs`; it remains purely proposed, not partially built.

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
  flake id with its canonical path (best-effort, since paths can move). Does not
  exist today.
- Specify the **devshell-likelihood gate** (current behaviour, now specified for
  the first time): a flake whose `flake.nix` contains none of
  `devShells`/`devShell`/`mkShell` is out of scope for the trust flow entirely;
  no prompt is ever shown for it, trusted or not.
- Specify the **nested child-flake trust** interaction (current behaviour, now
  specified for the first time): walking from a trusted parent's subshell into a
  directory owned by a different, nested `flake.nix` triggers an independent
  trust check on the child. A trusted child spawns its own nested subshell; an
  untrusted or suppressed child leaves the user in the parent's subshell with no
  additional prompt.

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
- **Breaking?**: No. Existing marker layout is preserved. Most of this delta
  specifies behaviour that already exists in `trust.rs`/`prompt.rs`; three
  pieces are genuinely new behaviour to implement, not formalisation of an
  existing intent: forced `0600`/`0700` permissions, single-prompt throttling,
  and `trust --list`. None of the three change what is already trusted or
  untrusted today.
- **Migration**: None. Existing markers under `dirs::trust_dir()` remain valid.

## Success Criteria

- **SC-T-001**: Walking into an untrusted flake with the prompt hook installed
  does NOT spawn a daemon and does NOT run `nix eval` at prompt time.
- **SC-T-002** (not yet met: `trust.rs` does not call `set_permissions` today):
  `xi develop trust <path>` writes exactly one marker file at
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
- **SC-T-007** (not yet met: `prompt.rs::handle_parent` prints the warning on
  every untrusted tick today, with no throttle key): The trust prompt for an
  untrusted flake is the _only_ interactive-stderr write the prompt hook
  produces; simulated five-minute daemon outage + walking through the untrusted
  flake produces exactly one stderr line (the trust prompt), not per-tick.
- **SC-T-008** (already true today, newly specified): walking from inside a
  trusted parent's subshell into a nested, trusted child flake spawns a nested
  subshell for the child; walking into a nested, untrusted child flake leaves
  the user in the parent's subshell with no additional prompt, and the per-child
  suppress flag clears once the user `cd`s back to the parent.
- **SC-T-009** (already true today, newly specified): a `flake.nix` containing
  none of `devShells`, `devShell`, or `mkShell` never triggers a trust prompt,
  trusted or not; the trust flow is skipped entirely for it.

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
