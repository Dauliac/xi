# Proposal

## Why

"Which mechanism does this host run" is two different questions wearing one
name. `SystemDeploymentKind::detect()` (`crates/xi-core/src/sources/deployment.rs`)
only ever looks at host state (`/etc/NIXOS`, the system-manager profile path,
`cfg!(target_os = "macos")`), which is reliable once something is already
installed and has nothing to say on a bare host - exactly the host a fresh
bring-up needs an answer for. And Scope::User isn't a fourth mechanism
parallel to the other three at all: home-manager can be embedded inside the
System-scope activation (`home-manager.users.<name>` inside
`nixosConfigurations.<host>`, registered as a `home-manager-<user>.service`
unit `deployment.rs::embedded_home_manager_unit` already detects), fully
standalone (`homeConfigurations.<name>`, its own profile), both at once, or
neither. Nothing today reads that signal before an activation runs; it only
surfaces as a warning from `xi home status`, after the fact.

Separately, `push::discover::TargetSource` has five variants and none of them
is a system-manager host: `Deploy`, `ColmenaHive`, `NixosConfigurations`,
`DarwinConfigurations`, `HomeConfigurations`. A system-manager box is an
entirely ordinary local `xi system switch` target; it is structurally
invisible to a fleet rollout, which has no flake-output namespace to
enumerate it from.

## What Changes

- **OS-scope mechanism** (`Scope::System`; exactly one of `nixos` | `darwin` |
  `system-manager`): when host-state probing is silent (nothing installed
  yet, or genuinely ambiguous), detection falls through to the sources before
  giving up. Does the adopted (or about-to-be-adopted) flake declare this
  host's name under `nixosConfigurations` or under `systemConfigs` - the same
  hostname match `resolve_toplevel` already performs to pick *which* host's
  config to build, run one step earlier to also pick *which namespace* it's
  in. Exactly one match resolves the mechanism with no host state at all;
  both or neither is reported as a real ambiguity, not guessed.
- **User-scope activation mode** (`Scope::User`; independently embedded,
  standalone, both, or neither - not a fourth mechanism): resolved from
  `embedded_home_manager_unit(username)` (existing) crossed with whether
  `homeConfigurations.<user>@<host>` or `homeConfigurations.<user>` is
  discoverable (new, host-qualified name checked first). `Both` surfaces the
  existing `status` warning text, but now *before* an activation decision is
  made rather than only after one, as a `status`-only side note.
- `push::discover::TargetSource` gains a sixth variant, `SystemConfigs`,
  mapping to a new `FlakeOutput::SystemConfigs`-based discovery pass mirroring
  `NixosConfigurations`'s. System-manager has no uniform `<tool> switch
  --flake <ref>#<name>` shape the way `nixos-rebuild`/`darwin-rebuild`/
  `home-manager` do (`crates/xi/src/system/mod.rs`'s own local activation is a
  bespoke build-toplevel, `nix build --profile <path> --set`,
  run-`bin/activate` sequence, not a CLI subcommand), so a discovered
  `SystemConfigs` target is built with `ProfileKind::Custom`'s existing
  `activate_command` field carrying that same sequence, rather than
  introducing a new `ProfileKind` variant.

## Capabilities

### New Capabilities

- `xi-deploy-mechanism-detection`: resolves, for the local host, which
  OS-scope mechanism applies (host state, falling through to the sources) and
  which User-scope activation mode applies (embedded, standalone, both, or
  neither); and makes a system-manager host a discoverable fleet-push target.

### Modified Capabilities

(none - greenfield; `SystemDeploymentKind` and `push::discover` are not yet
living specs under `openspec/specs/`.)

## Impact

- **Affected code**: `crates/xi-core/src/sources/deployment.rs`
  (`SystemDeploymentKind::detect`'s sources-fallback rung,
  `embedded_home_manager_unit` reused for a pre-activation decision, not only
  `status`), `crates/xi-deploy/src/push/discover.rs` (`TargetSource::SystemConfigs`,
  `ProfileKind::Custom` population for it), `crates/xi-core/src/flake_output.rs`
  (a `SystemConfigs` taxonomy variant, if one does not already exist for the
  local-only path `system/mod.rs` uses).
- **Coordination**: `xi-deploy-unified-verb` calls this capability to resolve
  `--mechanism` and the `--scope all` / embedded-overlap behavior, and to
  answer Fig. "which system mechanism does this host run" in its decision
  tree. `xi-deploy-unify-activation`'s entrypoint is what actually runs once
  this capability has picked a mechanism.
- **Breaking?**: No. Purely additive: a new enum variant, a new fallback rung
  consulted only when host state is already silent, and a pre-activation read
  of a check `status` already performs.
- **Migration**: None.

## Success Criteria

- **SC-MD-001**: On a host with no `/etc/NIXOS` and no system-manager profile
  yet, whose adopted flake declares `nixosConfigurations.<hostname>` and no
  `systemConfigs.<hostname>`, mechanism detection resolves `nixos` with no
  host-state marker present.
- **SC-MD-002**: The same host, with the flake instead declaring only
  `systemConfigs.<hostname>`, resolves `system-manager`.
- **SC-MD-003**: The same host, with the flake declaring the hostname under
  *both* namespaces, refuses and names the ambiguity rather than picking one.
- **SC-MD-004**: A user with a `home-manager-<user>.service` unit present and
  no discoverable `homeConfigurations.<user>@<host>` or `.<user>` resolves to
  `Embedded`.
- **SC-MD-005**: A user with both the unit present and a discoverable
  standalone output resolves to `Both`, surfacing the existing overlap
  warning.
- **SC-MD-006**: A flake declaring `systemConfigs.web3` is discovered by
  `xi push` as a target named `web3` with `ProfileKind::Custom` carrying the
  build-toplevel/profile-set/activate sequence, not a `nixos-rebuild`- or
  `darwin-rebuild`-shaped command.
