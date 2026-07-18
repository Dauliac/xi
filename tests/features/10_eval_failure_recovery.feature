Feature: Evaluation failure and recovery
  As a developer using xi develop
  I want the daemon to handle eval failures gracefully
  So that my devshell keeps working with the last-good env and recovers when I fix the error

  Background:
    Given subshell-A is active in "~/projectA"
    And the daemon is in state "Ready" with a valid env

  Scenario: Syntax error in flake.nix
    When the user introduces a syntax error in flake.nix
    And the watcher detects the change
    And the daemon re-evaluates
    And "nix print-dev-env" fails with a syntax error
    Then the daemon state transitions to BuildFailed:
      | Field       | Value                  |
      | error       | "error: syntax error..." |
      | retry_count | 1                      |
      | next_retry  | now + 30s              |
    And the daemon does NOT update env files or bump generations
    And the daemon pushes an error notification: "devshell failed: error: syntax error..."
    And the subshell continues working with the last-good env
    And on the next prompt, stderr shows the error notification

  Scenario: Exponential backoff retry
    Given the daemon is in state BuildFailed with retry_count=1
    And no file changes are detected
    When 30 seconds pass
    Then the daemon retries the evaluation automatically
    And if the eval still fails:
      | retry_count | backoff_delay |
      | 1           | 30s           |
      | 2           | 60s           |
      | 3           | 120s          |
      | 4           | 240s          |
      | 5+          | 300s (cap)    |
    And the error notification is only re-pushed if the error message changed

  Scenario: Recovery after fixing the error
    Given the daemon is in state BuildFailed with retry_count=3
    When the user fixes the syntax error in flake.nix
    And the watcher detects the change
    Then the daemon resets backoff (retry_count=0, clears last_error_time)
    And the daemon re-evaluates immediately
    And the eval succeeds
    And the daemon transitions to state "Ready"
    And the daemon bumps generations and writes files
    And the daemon pushes notification: "devshell recovered"
    And the next prompt re-sources the env

  Scenario: File change during backoff resets retry
    Given the daemon is in state BuildFailed with retry_count=3 and next_retry in 200s
    When the watcher detects a file change
    Then the daemon resets backoff immediately
    And the daemon re-evaluates without waiting for the backoff timer
