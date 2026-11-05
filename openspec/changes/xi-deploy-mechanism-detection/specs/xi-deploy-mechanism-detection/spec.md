# Capability: xi-deploy-mechanism-detection

## ADDED Requirements

### Requirement: OS-scope mechanism falls through to the sources when host state is silent

Mechanism detection for `Scope::System` MUST first consult host state in this
order: an explicit override, then `cfg!(target_os = "macos")`, then
`/etc/NIXOS`, then the system-manager profile path. When none of those
resolve a mechanism, detection MUST consult the adopted or about-to-be-adopted
sources: if exactly one of `nixosConfigurations.<hostname>` or
`systemConfigs.<hostname>` is declared for this host's name, that namespace's
mechanism resolves. If both are declared, or neither, detection MUST refuse
and name the ambiguity rather than guessing.

#### Scenario: A bare host resolves from sources alone

- **GIVEN** a host with no `/etc/NIXOS` and no system-manager profile, not
  running macOS
- **AND** the adopted flake declares `nixosConfigurations.<hostname>` and no
  `systemConfigs.<hostname>`
- **WHEN** mechanism detection runs
- **THEN** it resolves `nixos`

#### Scenario: Host state wins when present

- **GIVEN** a host with `/etc/NIXOS` present
- **WHEN** mechanism detection runs, regardless of what the sources declare
- **THEN** it resolves `nixos` from host state, without consulting the
  sources

#### Scenario: An ambiguous declaration refuses

- **GIVEN** a bare host whose adopted flake declares the hostname under both
  `nixosConfigurations` and `systemConfigs`
- **WHEN** mechanism detection runs
- **THEN** it exits non-zero, naming both namespaces, rather than picking one

### Requirement: User-scope activation mode is independent of the OS-scope mechanism

Activation-mode resolution for `Scope::User` MUST evaluate two independent
signals for the target username: whether a `home-manager-<user>.service`
systemd unit is present (embedded), and whether a `homeConfigurations.<user>@<host>`
or `homeConfigurations.<user>` output is discoverable in the sources
(standalone), checking the host-qualified name first. The four combinations
MUST resolve to `Embedded`, `Standalone`, `Both`, or `None` respectively; none
of the four is itself a fifth OS-scope mechanism.

#### Scenario: Embedded only

- **GIVEN** `home-manager-alice.service` is present
- **AND** no `homeConfigurations.alice@<host>` or `homeConfigurations.alice`
  is discoverable
- **WHEN** activation-mode resolution runs for `alice`
- **THEN** it resolves `Embedded`

#### Scenario: Standalone only

- **GIVEN** no `home-manager-alice.service` is present
- **AND** `homeConfigurations.alice` is discoverable
- **WHEN** activation-mode resolution runs for `alice`
- **THEN** it resolves `Standalone`

#### Scenario: Both, surfaced before activation

- **GIVEN** `home-manager-alice.service` is present
- **AND** `homeConfigurations.alice@<host>` is also discoverable
- **WHEN** activation-mode resolution runs for `alice`, before any `xi home
  switch` activation proceeds
- **THEN** it resolves `Both` and surfaces the overlap warning text
  `deployment.rs` already uses in `status`

### Requirement: A system-manager host is a discoverable push target

`push::discover::TargetSource` MUST include a `SystemConfigs` variant,
discovered from a `systemConfigs` flake output the same way
`NixosConfigurations` is discovered from `nixosConfigurations`. A target
discovered this way MUST be built with `ProfileKind::Custom`, carrying the
build-toplevel / `nix build --profile <path> --set` / run-`bin/activate`
sequence `xi system switch` already runs locally, not a `nixos-rebuild`- or
`darwin-rebuild`-shaped invocation.

#### Scenario: A system-manager target is discovered and activated

- **GIVEN** a flake declaring `systemConfigs.web3`
- **WHEN** `xi push` runs with no explicit targets
- **THEN** `web3` appears in the discovered target set with
  `ProfileKind::Custom`'s `activate_command` set to the build/profile/activate
  sequence, and a push to it runs that sequence rather than
  `nixos-rebuild switch` or `darwin-rebuild switch`
