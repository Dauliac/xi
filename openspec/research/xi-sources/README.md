# xi sources - research + design index

This directory holds the research notes and spec-link map for the "xi as
universal pull/push for Nix" effort.

## Research notes (primary sources, cite-rich)

- [pull-gitops.md](./pull-gitops.md) - comin, krops, morph, deploy-rs, colmena,
  system.autoUpgrade. Convergent patterns + steal list.
- [push-deploy.md](./push-deploy.md) - deploy-rs, colmena, morph, nixinate,
  krops, nixos-anywhere, nixops4. Ten specific gaps in xi's `builtin` backend.
- [bootstrap-curl-pipe-sh.md](./bootstrap-curl-pipe-sh.md) - DetSys
  `nix-installer`, Lix installer, official Nix script, nixos-anywhere, rustup,
  Starship. Two-stage pattern, inline-SHA256 pinning.
- [state-layout.md](./state-layout.md) - XDG spec, FHS, systemd StateDirectory,
  chezmoi, yadm, ansible-pull, Argo CD. Recommended paths table.
- [xi-codebase-map.md](./xi-codebase-map.md) - insertion points in the xi Rust
  workspace: switch sites, xi-deploy trait, xi-develop daemon, config loader,
  Nix modules, FlakeOutput taxonomy, trust model.

## Positioning: xi as a three-way hybrid

Operator-facing framing: xi becomes the single tool across three previously
separate patterns.

1. **Pull-mode GitOps** (comin-style), owned by [[xi-sources-gitops]]. The
   daemon writes only to the sources dir.
2. **Push-mode deploy** (ansible / deploy-rs / colmena style), owned by
   [[xi-sources-push]]. The operator pushes from their workstation to one or
   more remote hosts over SSH.
3. **Chezmoi-style hermetic sync**, owned by [[xi-sources-conflict]]'s
   `xi sources sync` verb. The sources dir is the authority; the working dir is
   a copy-with-erase destination. Reverse flow goes through `git push` plus the
   daemon's next fetch, never through xi.

Naive `git pull` into the working dir is explicitly forbidden by the model's
sanctioned-write-flow set, because it introduces merge conflicts and pulls files
the working tree never wanted to track. The sync verb exists specifically to
avoid that failure mode.

For the bootstrap flow, phase 2 (Nix install) delegates to one of two installer
backends selected by the `installer` config key: `detsys` (Determinate Systems
installer, default on non-NixOS Linux and macOS) or `lix` (recommended when
`cache.lix.systems` appears in the resolved flake's declared substituters). The
`none` value opts out entirely for configuration-management workflows where Nix
is pre-installed.

## OpenSpec changes produced from this research

Twelve proposals under `openspec/changes/` compose into one feature area. The
first seven (model, config, conflict, gitops, push, cache-prestage, bootstrap)
are the primary design surface. The last five (nix-modules, credentials, race,
crash-recovery, observability) were added in a second-pass "missing pieces"
sweep. Dependency arrows below reflect which spec a given spec reads from.

```
xi-sources-model (foundation: sources/state/working dirs, scope, ref lineage)
      |-- xi-sources-config         (paths, .xi.toml schema, env/CLI/Nix precedence)
      |       |-- xi-sources-gitops         (pull daemon reads config, enforces conflict)
      |       |-- xi-sources-push           (push verb reads sources context, closes backend gaps)
      |       |-- xi-sources-cache-prestage (reads flake nix.settings via pure eval)
      |       |       `-- xi-sources-bootstrap    (curl|sh plus planner using prestage + model)
      |       `-- xi-sources-nix-modules    (full xi.sources.* option tree, three-head parity)
      |               |-- xi-sources-credentials  (ssh/https/oauth/anonymous fallthrough)
      |               `-- xi-sources-observability (tracing, Prometheus, healthz, OTLP, redaction)
      `-- xi-sources-conflict      (three-ref state matrix, local-wins policy, chezmoi sync)
              |-- xi-sources-gitops                (daemon enforces matrix on every tick)
              `-- xi-sources-race                  (advisory lock, pause/resume, priority queue)
                      `-- xi-sources-crash-recovery (transaction wrapper, shadow write, xi sources repair)
```

### Second-pass additions and what they lock in

- **xi-sources-nix-modules**: the full `xi.sources.*` attribute tree across
  `nixos`, `home-manager`, and `flake-parts` heads with a parity rule and
  activation scripts. Named as the authoritative registry so every other spec
  reads from one place.
- **xi-sources-credentials**: four credential kinds (`ssh`, `https`, `oauth`,
  `anonymous`) with a fixed fallthrough chain, a one-shot HTTPS credential
  helper that zeroes buffers on exit, TOFU limited to the bootstrap flow, and an
  opaque newtype that refuses `Serialize` so credentials never leak through
  logs.
- **xi-sources-race**: a per-sources-id advisory lock with a JSON lock record,
  dead-pid steal protocol with a 10-second grace, a daemon control-socket
  `pause`/`resume` RPC for inbound push, and a strict priority queue (switch
  over sync over push over bootstrap over daemon).
- **xi-sources-crash-recovery**: transaction wrapper per actor, shadow-write for
  the sources dir via `.incoming` and `.previous` rename choreography, atomic
  metadata writes, a `xi sources repair` verb (idempotent, with `--dry-run`),
  auto-repair on daemon startup before lock acquisition, and receipt
  reconstruction from on-disk evidence.
- **xi-sources-observability**: structured tracing per tick (one root plus five
  fixed phase child spans), journald for system scope and daily-rotated ndjson
  for user scope, operator readouts (`xi sources log tail`,
  `xi sources events`), an opt-in Prometheus exporter with the full metric name
  list, `/healthz` for container probes, opt-in OTLP export, and a redaction
  rule that composes with the credentials spec.

## Beads DAG

The whole effort is also encoded as a bead DAG - the plan is stamped by
`scripts/beads-plan-xi-sources.sh`. Run it after unsticking the local Dolt
backend (currently `.beads/dolt` is a sqlite file where newer `bd` wants a
directory). The script produces one bead per research doc, one per spec, plus a
validation and operator-review gate.

## Open questions carried forward

Each research doc has its own open-questions section. The big five needing
operator input before implementation starts:

1. Signing model for the gitops daemon - SSH `allowed_signers`, GPG, both,
   neither? (pull-gitops.md)
2. Default push mode - closure vs sources? (push-deploy.md)
3. Where does the inline SHA-256 for `bootstrap.sh` get templated -
   release-please step, Nix build output, manual? (bootstrap doc)
4. Working-dir discovery UX - chezmoi-style "pointer in state" wins over an
   environment variable, but which do we default to? (state-layout.md)
5. Should xi vendor DetSys's `nix-installer` crate or shell-exec it? (bootstrap
   doc) - the proposal starts with shell-exec; vendor is a P4 follow-up.

These are explicitly deferred to the operator-review gate (bead
`phase:review,blocking:human`), not resolved in the specs above.
