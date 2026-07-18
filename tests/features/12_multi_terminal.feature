Feature: Multiple terminals in the same flake
  As a developer with multiple terminal windows
  I want each terminal to independently track devshell state
  So that notifications appear once per terminal and env updates apply everywhere

  Background:
    Given terminal-1 has subshell (PID 1001) in "~/projectA"
    And terminal-2 has subshell (PID 2001) in "~/projectA"
    And the daemon has 2 registered consumers

  Scenario: Both terminals receive notifications
    When flake.nix changes and the daemon re-evaluates
    And the eval succeeds with new packages
    Then the daemon pushes the notification to the global queue
    And the daemon marks both PID 1001 and PID 2001 as needing re-source
    And terminal-1's next prompt queries daemon -> gets notification + source command
    And terminal-2's next prompt queries daemon -> gets notification + source command
    And the notification is shown exactly once per terminal (cursor-based dedup)

  Scenario: One terminal exits, daemon stays alive
    When terminal-1's subshell exits (user closes terminal)
    And daemon deregisters PID 1001 (shell_count: 2 -> 1)
    Then the daemon does NOT begin shutdown (shell_count > 0)
    And terminal-2 continues operating normally

  Scenario: Consumer registration skips old notifications
    Given the daemon has pushed notifications [A, B, C]
    And terminal-1 has already drained all three
    When terminal-3 opens a new subshell (PID 3001) in "~/projectA"
    And the daemon registers PID 3001
    Then PID 3001's cursor starts at the current tail
    And terminal-3 does NOT see old notifications [A, B, C]
    And terminal-3 only sees future notifications

  Scenario: Dead consumer reaping
    Given terminal-1's shell process (PID 1001) has crashed without sending exit
    When the daemon periodically reaps dead consumers
    And checks if PID 1001 is alive (via /proc/{pid}/stat on Linux)
    And PID 1001 is dead
    Then the daemon removes PID 1001 from the consumer list
    And shell_count decrements
