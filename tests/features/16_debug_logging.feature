Feature: Debug logging across all async processes
  As a developer debugging xi develop
  I want comprehensive logging when XI_LOG=debug is set
  So that I can trace all async operations and diagnose issues

  Scenario: Debug logging in prompt command
    Given XI_LOG=debug is set
    When "xi develop prompt" runs
    Then stderr includes tracing spans:
      | Component | Example log                                              |
      | prompt    | DEBUG nh_develop::prompt: walk_up found flake at ~/projectA |
      | prompt    | DEBUG nh_develop::prompt: daemon connect 0.3ms           |
      | prompt    | DEBUG nh_develop::prompt: daemon response: should_source_env=true |

  Scenario: Debug logging in daemon
    Given XI_LOG=debug is set for the daemon process
    When the daemon runs
    Then daemon logs include:
      | Component | Example log                                              |
      | daemon    | DEBUG nh_develop::daemon: eval started for target=default |
      | daemon    | DEBUG nh_develop::daemon: nix print-dev-env completed in 2.1s |
      | daemon    | DEBUG nh_develop::daemon: env_hash changed, writing files |
      | daemon    | DEBUG nh_develop::daemon: watcher: flake.nix modified    |
      | daemon    | DEBUG nh_develop::daemon: consumer registered PID 1001   |
      | daemon    | DEBUG nh_develop::daemon: consumer deregistered PID 1001 |
    And daemon logs go to the daemon's stderr (journald or log file)

  Scenario: Debug logging in subshell prompt hook
    Given XI_LOG=debug is set
    When "xi develop prompt --subshell" runs inside the subshell
    Then stderr includes:
      | Log entry                                             |
      | DEBUG checking trust for flake_id=abcdef1234567890    |
      | DEBUG querying daemon at /tmp/xi-1000/abcdef.../daemon.sock |
      | DEBUG daemon response: should_source_env=false, notifications=1 |
      | DEBUG generating shell output: 0 source commands, 1 notification |

  Scenario: No debug output when XI_LOG is unset
    Given XI_LOG is not set
    When any xi develop command runs
    Then no DEBUG lines appear on stderr
    And only user-facing notifications appear on stderr
