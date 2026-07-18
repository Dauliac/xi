Feature: Version upgrade handling
  As a developer who upgrades xi
  I want the daemon to restart with the new version
  So that I always run matching client/daemon versions

  Scenario: Binary version mismatch triggers daemon restart
    Given daemon v4.4.0 is running for "~/projectA"
    And the user upgrades xi to v4.5.0
    When "xi develop prompt" connects to the daemon
    And queries status -> version "4.4.0" != CARGO_PKG_VERSION "4.5.0"
    Then the Rust binary sends a Shutdown request to the old daemon
    And waits for the old daemon to stop (200ms)
    And starts a new daemon (v4.5.0)
    And the new daemon checks CACHE_VERSION:
      | Condition           | Action                      |
      | CACHE_VERSION same  | Warm-start from meta.json   |
      | CACHE_VERSION changed | Nuke state dir, fresh eval |
    And normal operation resumes with the new daemon

  Scenario: Cache version mismatch nukes state
    Given the daemon state dir contains files from CACHE_VERSION=1
    When a new daemon starts with CACHE_VERSION=2
    Then the daemon detects the version mismatch via the VERSION file
    And the daemon removes all files in the state dir (except VERSION)
    And the daemon writes the new VERSION file
    And the daemon performs a fresh evaluation from scratch

  Scenario: xi-bin file is updated on shell init
    Given the activation script runs during shell initialization
    When eval "$(xi develop activate zsh)" executes
    Then the Rust binary path is resolved and persisted to "$XDG_STATE_HOME/xi/develop/xi-bin"
    And subsequent daemon starts use this path to find the correct xi binary
