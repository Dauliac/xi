Feature: Live update when shellHook changes
  As a developer using xi develop
  I want my shell aliases and functions to update when I change shellHook
  So that I can iterate quickly on my development environment

  Background:
    Given subshell-A is active in "~/projectA"
    And the daemon is in state "Ready"

  Scenario: shellHook changed without package changes
    When the user edits shellHook in flake.nix
    And the daemon re-evaluates
    And nix store paths (packages) are unchanged but shellHook content differs
    Then the daemon writes new hook file using A/B slot switching
    And the daemon bumps hook-generation
    And the daemon does NOT bump env-generation (paths unchanged)
    And on the next prompt:
      | Step | Action                                           |
      | 1    | Daemon responds: should_source_hook=true          |
      | 2    | Daemon responds: should_source_env=false           |
      | 3    | Rust outputs source command for hook file only     |
    And new aliases and functions from shellHook are available
    And PATH is NOT re-sourced (unchanged)

  Scenario: Both hook and packages change simultaneously
    When the user edits flake.nix changing both packages and shellHook
    And the daemon re-evaluates
    Then the daemon bumps both env-generation AND hook-generation
    And on the next prompt, both env and hook files are re-sourced
    And new packages and new aliases are both available

  Scenario: Hook with aliases and functions works
    Given the shellHook contains:
      """
      alias ll='ls -la'
      my_func() { echo "hello from devshell"; }
      """
    When the daemon writes the hook file
    And the subshell sources it via the prompt hook
    Then the alias "ll" is available in the subshell
    And the function "my_func" is available in the subshell
    And shell completions defined in the hook are available
