Feature: Nested flakes in a monorepo
  As a developer working in a monorepo with multiple flakes
  I want devshells to nest correctly when I cd into sub-flakes
  So that I have tools from both the parent and child flakes

  Background:
    Given a parent shell with activation loaded
    And "~/mono" contains flake.nix (flakeA, trusted)
    And "~/mono/services/api" contains flake.nix (flakeB, trusted)

  Scenario: Enter nested flake from within parent flake
    Given subshell-A (PID 1001) is active in "~/mono" (flakeA)
    When the user runs "cd services/api"
    And subshell-A's prompt hook fires
    And "xi develop prompt --subshell" detects new flake at "~/mono/services/api"
    Then Rust outputs a subshell spawn command for flakeB to stdout
    And subshell-A evals it, spawning subshell-B (PID 1002)
    And subshell-A blocks (waiting for subshell-B)
    And daemon-B starts for flakeB
    And subshell-B sources flakeB env + hook
    And subshell-B's PATH is composed as: B_paths : A_paths : GLOBAL_PATH
    And daemon-B registers PID 1002
    And daemon-A still has PID 1001 registered (waiting but alive)

  Scenario: Leave nested flake back to parent
    Given subshell-B (PID 1002) is nested inside subshell-A (PID 1001)
    And subshell-B is in "~/mono/services/api" (flakeB)
    When the user runs "cd ../.." (now in "~/mono", flakeA root)
    And subshell-B's prompt hook detects CWD is outside flakeB
    Then subshell-B exits (exit 0)
    And daemon-B deregisters PID 1002 (shell_count -> 0, begins shutdown)
    And subshell-A resumes
    And subshell-A's prompt hook fires
    And subshell-A is still inside flakeA -> normal operation
    And PATH is back to A_paths : GLOBAL_PATH

  Scenario: Direct cd into deeply nested flake from outside
    Given the parent shell has no devshell active
    When the user runs "cd ~/mono/services/api"
    And the parent prompt hook fires
    And "xi develop prompt" walks up from CWD:
      | Path                   | flake.nix? |
      | ~/mono/services/api    | yes (flakeB) |
      | ~/mono/services        | no           |
      | ~/mono                 | yes (flakeA) |
    And detects flake stack: [flakeA, flakeB] (outermost first)
    Then Rust outputs subshell spawn for flakeA (outermost only)
    And subshell-A starts, initializes with flakeA env
    And subshell-A's init script cd's to "~/mono/services/api" (original CWD)
    And subshell-A's prompt hook fires
    And detects flakeB at CWD -> spawns subshell-B
    And subshell-B has PATH = B_paths : A_paths : GLOBAL_PATH
    And the user is now in "~/mono/services/api" with both devshells loaded

  Scenario: Leave all nested flakes at once
    Given subshell-B is nested inside subshell-A
    And the user is in "~/mono/services/api"
    When the user runs "cd ~" (outside both flakes)
    Then subshell-B detects CWD is outside flakeB -> exits
    And subshell-A resumes at "~"
    And subshell-A detects CWD is outside flakeA -> exits
    And parent shell resumes at "~"
    And both daemons begin graceful shutdown
