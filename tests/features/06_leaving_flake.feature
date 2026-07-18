Feature: Leaving a flake directory
  As a developer using xi develop
  I want my devshell to cleanly deactivate when I cd out of the flake
  So that my shell environment is clean and I don't have stale tools in PATH

  Background:
    Given subshell-A with PID 1001 is active in "~/projectA"
    And the daemon for "~/projectA" has shell_count=1

  Scenario: User cd's out of flake directory
    When the user runs "cd ~"
    And the subshell prompt hook fires
    And "xi develop prompt --subshell" detects CWD "~" is outside "~/projectA"
    Then Rust outputs "exit 0" to stdout
    And subshell-A begins exit sequence
    And the EXIT trap fires: "xi develop prompt --exit --pid 1001"
    And the daemon deregisters PID 1001 (shell_count: 1 -> 0)
    And subshell-A exits with status 0
    And the parent shell resumes
    And the parent shell's environment is perfectly clean (subshell boundary)
    And the parent shell's CWD is "~" (where the user cd'd)

  Scenario: Daemon shuts down after last shell exits
    Given subshell-A exits and shell_count reaches 0
    Then the daemon waits for a grace period (60 seconds)
    And if no new consumers register during the grace period
    Then the daemon shuts down gracefully
    And the daemon socket file is removed
    And the daemon PID file is removed
    And GC roots persist on disk (survive daemon restarts)

  Scenario: User cd's to parent directory one level at a time
    Given the user is in "~/projectA/src/lib"
    When the user runs "cd .."
    Then the subshell detects CWD "~/projectA/src" is still inside "~/projectA"
    And the subshell continues normally (no exit)
    When the user runs "cd ../.."
    Then the subshell detects CWD "~" is outside "~/projectA"
    And the subshell exits (exit 0)
    And the parent resumes at "~"
