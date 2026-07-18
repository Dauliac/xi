Feature: Live update when a package is added
  As a developer using xi develop
  I want my devshell to automatically update when I add a package to flake.nix
  So that new tools are available without restarting the shell

  Background:
    Given subshell-A is active in "~/projectA" with PID 1001
    And the daemon is in state "Ready"
    And the current env has packages [cargo, gcc]

  Scenario: Package added triggers re-source
    When the user edits flake.nix adding "python" to devShells
    And the file watcher detects the change
    And the daemon re-evaluates via "nix print-dev-env --json"
    And the evaluation succeeds with a different env_hash
    Then the daemon writes new env files using A/B slot switching
    And the daemon bumps env-generation from N to N+1
    And the daemon updates per-PID state: PID 1001 needs re-source
    And the daemon pushes a global notification: kind=Success, "devshell updated: + python 3.12"
    And on the next prompt in subshell-A:
      | Step | Action                                                          |
      | 1    | "xi develop prompt --subshell" queries daemon with "--pid 1001" |
      | 2    | Daemon responds with should_source_env=true and env_file_path   |
      | 3    | Rust outputs "source '/path/to/env.sh'" to stdout              |
      | 4    | Rust outputs the notification to stderr                         |
    And "python" is now available in PATH

  Scenario: Package removed triggers re-source with cleanup
    Given the current env has packages [cargo, gcc, python]
    When the user edits flake.nix removing "python" from devShells
    And the daemon re-evaluates successfully
    Then the new env file's cleanup preamble unsets previously-injected vars
    And re-exports only the remaining vars
    And "python" is no longer in PATH
    And the daemon pushes notification: "devshell updated: - python 3.12"

  Scenario: Package version updated
    Given the current env has packages [cargo-1.94.0]
    When flake.lock is updated causing cargo to become 1.95.0
    And the daemon re-evaluates
    Then the daemon detects the version change via env_hash diff
    And pushes notification: "devshell updated: ~ cargo 1.94.0 -> 1.95.0"
    And the subshell re-sources the env file on next prompt
