# Tasks

Live state in beads under `change:xi-deploy-mechanism-detection`.

## 1. OS-scope mechanism: sources fallback

- [ ] 1.1 Extract `SystemDeploymentKind::detect`'s host-state checks unchanged;
      add a `None` fallthrough that consults the adopted/ about-to-be-adopted
      flake for `nixosConfigurations.<hostname>` and `systemConfigs.<hostname>`,
      reusing `resolve_toplevel`'s existing hostname-matching helper rather than
      a second implementation of it.
- [ ] 1.2 Unit test: host state present always wins, sources are never
      consulted.
- [ ] 1.3 Unit test: host state silent, exactly one namespace declares the
      hostname, that namespace's mechanism resolves.
- [ ] 1.4 Unit test: host state silent, both or neither namespace declares the
      hostname, detection returns a named ambiguity error rather than a
      mechanism.

## 2. User-scope activation mode

- [ ] 2.1 Implement the four-way resolver (`Embedded` | `Standalone` | `Both` |
      `None`) combining `embedded_home_manager_unit` with a new
      `homeConfigurations.<user>@<host>` / `.<user>` discovery check,
      host-qualified name checked first.
- [ ] 2.2 Unit test each of the four combinations against a fixture flake output
      set and a fixture filesystem for the systemd unit check.
- [ ] 2.3 Wire the `Both` case to the existing `status` overlap-warning text so
      it is reachable from the new resolver, not duplicated; verify the warning
      text is identical whether reached from `status` or from this resolver.

## 3. `SystemConfigs` push target

- [ ] 3.1 Add `TargetSource::SystemConfigs` to `push::discover`, discovered from
      a `systemConfigs` flake output exactly as
      `TargetSource::NixosConfigurations` is discovered today; add the taxonomy
      variant to `xi-core`'s `FlakeOutput` if the local-only `system/mod.rs`
      path does not already expose one reusable here.
- [ ] 3.2 Populate a discovered `SystemConfigs` target's `ProfileKind::Custom`
      `activate_command` with the same build-toplevel /
      `nix build --profile
      <path> --set` / run-`bin/activate` sequence
      `xi system switch` runs locally; unit test the generated command string
      against that known sequence.
- [ ] 3.3 BDD: `xi push` against a flake declaring `systemConfigs.web3`
      discovers `web3` and a `builtin`-backend push to it runs the
      `ProfileKind::Custom` sequence, verified against a fixture `Transport`.

## Workflow follow-up

- Archive this change once `xi-deploy-unified-verb` has confirmed it can read
  both resolvers (OS-scope mechanism, User-scope activation mode) for its
  `--mechanism`/`--scope all` behavior without further changes to either.
