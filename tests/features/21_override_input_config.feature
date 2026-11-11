Feature: Configurable flake input overrides
  As a developer working on two flakes at once
  I want --override-input to be settable from config and the environment
  So that I do not retype an absolute path on every build, check and develop

  # Background on why a wrapper has to do this at all:
  # --override-input is the only nix mechanism that beats flake.lock. The
  # user registry, the global registry file and --override-flake were all
  # measured to lose to the lock. See
  # openspec/changes/xi-override-input-config/design.md.

  Scenario: Config-only override reaches nix
    Given "~/.config/xi/config.toml" sets, under "[build]"
      | key            | value                                          |
      | override_input | { nix-lib = "~/ghq/github.com/Dauliac/nix-lib" } |
    And no "--override-input" flag and no "XI_OVERRIDE_INPUT" are present
    When "xi nix build .#xi" runs
    Then nix receives "--override-input nix-lib path:/home/<user>/ghq/github.com/Dauliac/nix-lib"
    And the input resolves to the local checkout
    And the input locks as '{"type":"path"}' rather than '{"type":"git"}'

  Scenario: Nothing configured changes nothing
    Given no override is set in any config file, environment variable or flag
    When any command generates its nix argv
    Then the argv is identical to the argv generated before this capability existed

  Scenario: The lock file is never written as a side effect
    Given one or more overrides are applied
    When the command completes
    Then "flake.lock" on disk is byte-identical before and after

  Scenario: Two inputs overridden from the environment
    Given "XI_OVERRIDE_INPUT" is 'nix-lib=./a nix-vm-test=./b'
    When a build runs
    Then both inputs are overridden
    And the same value written across multiple lines produces the same result

  Scenario: A flakeref containing "=" survives parsing
    Given a pair whose value is "github:o/r?ref=main&rev=abc"
    When the environment variable is parsed
    Then the input name is the text before the first "="
    And the flakeref retains its own "=" characters intact

  Scenario: A malformed environment entry is an error, not a silent drop
    Given "XI_OVERRIDE_INPUT" contains a token with no "=" or an empty name
    When the cascade is resolved
    Then the command fails and names the offending token
    # Silently dropping it would hide a typo that changes what gets built.

  Scenario: A transitive input is addressable
    Given the config key is the quoted name "nix-lib/flake-parts"
    When the override is applied
    Then nix receives "--override-input nix-lib/flake-parts <ref>"
    And the name is filtered against the parent input "nix-lib" only

  Scenario: A local directory never reaches nix as a bare path
    Given the configured value is "~/ghq/github.com/Dauliac/nix-lib"
    When the override is applied
    Then nix receives it with the "path:" scheme
    And a newly created untracked file inside that checkout is visible to the build
    # A bare path locks as git and hard-errors with
    # "Path '<file>' in the repository ... is not tracked by Git".

  Scenario: A path containing a space is percent-encoded
    Given the resolved path contains a space
    When the override is applied
    Then the space is percent-encoded before the "path:" prefix is applied
    And the command succeeds rather than failing with a parse error that
      prepends the current working directory to the literal text "path:"

  Scenario: A URL value is not touched
    Given the configured value is "github:NixOS/nixpkgs/nixos-unstable"
    When the override is applied
    Then it is forwarded verbatim with no path resolution attempted

  Scenario: A bare word is not mistaken for a directory
    Given the configured value is "nixpkgs", with no "./" prefix
    When the override is applied
    Then it is treated as an indirect flakeref and forwarded verbatim

  Scenario Outline: Relative values resolve against the context they were written in
    Given a relative value "<value>" written in <source>
    When the override is applied
    Then it resolves against <base>

    Examples:
      | source                     | value       | base                        |
      | .xi.toml                   | ../nix-lib  | the file's own directory    |
      | .xi.local.toml             | ../nix-lib  | the file's own directory    |
      | XI_OVERRIDE_INPUT          | ./nix-lib   | the flake root              |
      | --override-input on the CLI | ../nix-lib | the process working directory |

  Scenario: A project config value does not depend on the current directory
    Given ".xi.local.toml" at the flake root sets 'nix-lib = "../nix-lib"'
    When a command runs from a nested subdirectory such as "crates/xi/"
    Then the value resolves relative to the flake root, not to "crates/xi/"

  Scenario: A CLI flag keeps nix's own semantics
    Given "--override-input nix-lib ../nix-lib" is typed at a prompt
    When the command runs
    Then it resolves against the process working directory
    # A path typed at a prompt should mean what the shell would mean.

  Scenario: A relative value in the global config is refused
    Given "~/.config/xi/config.toml" sets a relative path value
    When the cascade is resolved
    Then the command fails stating that a machine-global config requires an
      absolute or tilde-prefixed path

  Scenario: An unusable path fails with both spellings
    Given a value resolves to a directory that does not exist, or to one with
      no "flake.nix"
    When the cascade is resolved
    Then the error names the value exactly as written
    And the error names the absolute path it resolved to
    # Without both, a wrong base directory is undiagnosable.

  Scenario: A global entry is silent in unrelated repos
    Given "~/.config/xi/config.toml" overrides "nix-lib"
    And a build runs in a flake that has no "nix-lib" input
    When the command runs
    Then no override is passed
    And nix emits no "does not match any input" warning

  Scenario: flake update still updates the lock
    Given overrides are configured
    When "xi nix flake update" runs
    Then no override is injected
    And the suppression is reported on stderr
    And "flake.lock" is written as it would be without any configuration
    # --override-input implies --no-write-lock-file.

  Scenario: Store commands never receive flake flags
    Given a store-level command such as "nix store gc" is proxied
    When the argv is generated
    Then no override flag is added

  Scenario: A flag replaces only the input it names
    Given config overrides both "nix-lib" and "nix-vm-test"
    And "--override-input nix-lib <other>" is passed
    When the argv is generated
    Then "nix-lib" takes the flag's value
    And "nix-vm-test" keeps the configured value

  Scenario: A shadowed entry is not advertised
    Given the same input is set at two levels of the cascade
    When the argv is generated
    Then exactly one override for that input is emitted
    And only the winning value appears in the report

  Scenario: The report names the resolved path
    Given an override is applied from a relative or tilde-prefixed value
    When the command runs
    Then the reported line shows the absolute path that was actually used
    And every applied override is reported once per invocation
