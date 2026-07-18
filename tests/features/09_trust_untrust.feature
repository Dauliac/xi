Feature: Trust and untrust flakes
  As a developer using xi develop
  I want to control which flakes can activate devshells
  So that I don't run arbitrary shellHooks from untrusted repos

  Scenario: Trust a flake
    Given a directory "~/projectA" containing flake.nix
    And "~/projectA" is NOT trusted
    When the user runs "xi develop trust" in "~/projectA"
    Then a trust marker file is created at "$XDG_CONFIG_HOME/xi/develop/trusted/{flake_id}"
    And stderr shows success: "trusted ~/projectA"
    And on the next prompt hook, a subshell is spawned

  Scenario: Untrust a flake while no devshell is active
    Given "~/projectA" is trusted
    And no devshell is active for "~/projectA"
    When the user runs "xi develop untrust" in "~/projectA"
    Then the trust marker file is removed
    And stderr shows success: "untrusted ~/projectA"

  Scenario: Untrust a flake while devshell is active
    Given subshell-A (PID 1001) is active in "~/projectA"
    And "~/projectA" is trusted
    When the user runs "xi develop untrust" (from another terminal or same)
    And the trust marker file is removed
    And on the next prompt in subshell-A
    And "xi develop prompt --subshell" checks trust
    And the trust check fails
    Then Rust outputs "exit 0" to stdout
    And the subshell exits cleanly
    And the parent shell resumes at CWD
    And stderr shows warning: "devshell deactivated (untrusted)"

  Scenario: Untrust a flake with nested devshells
    Given subshell-B (flakeB) is nested inside subshell-A (flakeA)
    When the user runs "xi develop untrust" for flakeA
    And subshell-B's prompt hook cannot detect trust change directly
    But subshell-A will detect untrust when subshell-B eventually exits
    Then when subshell-B exits for any reason
    And subshell-A resumes and its prompt hook detects untrust
    And subshell-A exits
    And the parent shell resumes

  Scenario: Trust marker is per-flake-path
    Given "~/projectA" is trusted
    And "~/projectB" is NOT trusted
    When the user cd's to "~/projectA"
    Then a devshell subshell is spawned
    When the user exits and cd's to "~/projectB"
    Then no subshell is spawned
    And the trust warning is shown

  Scenario: Trust an already trusted flake
    Given "~/projectA" is already trusted
    When the user runs "xi develop trust" in "~/projectA"
    Then the trust marker file is overwritten (idempotent)
    And stderr shows success (no error)

  Scenario: Untrust an already untrusted flake
    Given "~/projectA" is NOT trusted
    When the user runs "xi develop untrust" in "~/projectA"
    Then stderr shows warning: "already untrusted"
    And no error occurs
