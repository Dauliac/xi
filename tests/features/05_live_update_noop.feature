Feature: No-op when non-nix files are edited
  As a developer using xi develop
  I want my devshell to not needlessly re-source when I edit non-nix files
  So that my prompt stays fast and I don't see spurious notifications

  Background:
    Given subshell-A is active in "~/projectA"
    And the daemon is in state "Ready"

  Scenario: README edit does not trigger re-evaluation
    When the user edits README.md
    And README.md does NOT match the watcher patterns (*.nix, flake.lock)
    Then the file watcher does NOT detect a change
    And the daemon does NOT re-evaluate
    And no generation bumps occur
    And the next prompt: daemon responds should_source_env=false, should_source_hook=false
    And Rust outputs nothing to stdout
    And no notifications are shown

  Scenario: Nix eval produces identical output (content-hash dedup)
    When the user edits flake.nix (formatting-only change, no semantic diff)
    And the watcher detects the change
    And the daemon re-evaluates
    And the new env_hash is identical to the previous env_hash
    Then the daemon skips file writes (A/B slot switch not needed)
    And the daemon does NOT bump env-generation or hook-generation
    And the daemon does NOT push any notification
    And the next prompt: no re-source, no notification
