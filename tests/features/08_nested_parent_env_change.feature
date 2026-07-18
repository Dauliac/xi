Feature: Parent flake env changes while nested child is active
  As a developer in a nested devshell
  I want the parent env update to propagate correctly
  So that my nested devshell gets the updated parent packages

  Background:
    Given subshell-B (PID 1002, flakeB) is nested inside subshell-A (PID 1001, flakeA)
    And both daemons are running and Ready

  Scenario: Parent flake PATH changes while child is active
    When flakeA's env changes (new package added)
    And daemon-A re-evaluates and bumps env-generation
    And daemon-A marks PID 1001 as needing re-source
    But PID 1001 (subshell-A) is blocked waiting for subshell-B
    Then the notification is queued for PID 1001
    And subshell-B continues unaffected (its daemon-B is independent)

    When the user eventually exits subshell-B (cd out or exit)
    And subshell-A resumes
    And subshell-A's prompt hook fires
    Then daemon-A responds: should_source_env=true
    And subshell-A re-sources flakeA env (gets new packages)
    And subshell-A detects CWD is still inside flakeB
    And subshell-A automatically re-spawns subshell-B
    And subshell-B now has PATH = B_paths : A_NEW_paths : GLOBAL_PATH

  Scenario: Parent flake hook-only change while child is active
    When flakeA's shellHook changes (no package change)
    And daemon-A bumps hook-generation only
    Then subshell-B continues unaffected
    And when subshell-B exits and subshell-A resumes
    Then subshell-A re-sources flakeA hook
    And re-spawns subshell-B if user is still in flakeB dir
