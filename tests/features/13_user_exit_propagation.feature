Feature: User exit propagation
  As a developer using xi develop
  I want explicit exit (Ctrl+D/exit) to close the terminal
  And cd-out to return to the parent shell
  So that exit behavior matches my expectations

  Scenario: User exits subshell with Ctrl+D (no nesting)
    Given subshell-A (PID 1001) is active, no nesting
    When the user presses Ctrl+D or types "exit"
    Then subshell-A exits with non-zero status (1)
    And the EXIT trap fires: "xi develop prompt --exit --pid 1001"
    And the daemon deregisters PID 1001
    And the parent shell receives the non-zero exit status
    And the parent shell's eval'd code propagates: "[ $__nh_status -ne 0 ] && exit $__nh_status"
    And the parent shell exits
    And the terminal closes

  Scenario: User exits nested subshell with Ctrl+D (back-propagation)
    Given subshell-B (PID 1002) is nested inside subshell-A (PID 1001)
    When the user presses Ctrl+D in subshell-B
    Then subshell-B exits with non-zero status (1)
    And daemon-B deregisters PID 1002
    And subshell-A receives the non-zero exit status
    And subshell-A's eval'd code propagates the exit
    And subshell-A exits with non-zero status
    And daemon-A deregisters PID 1001
    And the parent shell exits
    And the terminal closes

  Scenario: cd-out of flake does NOT propagate exit
    Given subshell-A (PID 1001) is active
    When the user runs "cd ~" (outside the flake)
    Then subshell-A exits with status 0
    And the parent shell receives exit status 0
    And the parent shell does NOT exit (status 0 means cd-out, not user exit)
    And the parent shell resumes at "~"

  Scenario: cd-out of nested flake only kills the inner subshell
    Given subshell-B (PID 1002) nested inside subshell-A (PID 1001)
    And the user is in "~/mono/services/api" (flakeB)
    When the user runs "cd ../.." (back to "~/mono", flakeA)
    Then subshell-B exits with status 0
    And subshell-A resumes (does NOT exit)
    And subshell-A's prompt hook fires normally
    And subshell-A is still inside flakeA

  Scenario: Ctrl+C does NOT exit the subshell
    Given subshell-A is active
    When the user presses Ctrl+C
    Then the currently running command is interrupted (if any)
    But the subshell itself does NOT exit
    And the prompt appears again normally

  Scenario: EXIT trap sends deregister on any exit
    Given subshell-A has an EXIT trap installed
    When the subshell exits for any reason (exit 0, exit 1, SIGTERM, etc.)
    Then the EXIT trap runs "xi develop prompt --exit --pid $$"
    And the daemon receives the deregister request
    And the daemon decrements shell_count for the flake
