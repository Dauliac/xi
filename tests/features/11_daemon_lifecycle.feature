Feature: Daemon lifecycle management
  As a developer using xi develop
  I want the daemon to start, stop, and restart reliably
  So that my devshell is always available and responsive

  Scenario: Daemon starts on first prompt hook
    Given no daemon is running for "~/projectA"
    When "xi develop prompt" runs for "~/projectA"
    And the flake is trusted
    Then the Rust binary spawns the daemon as a background process
    And the daemon binds a Unix socket at "/tmp/xi-{uid}/{flake_id}/daemon.sock"
    And the daemon writes a PID file at "/tmp/xi-{uid}/{flake_id}/daemon.pid"
    And the daemon state starts as "Starting"
    And the daemon transitions to "Evaluating" immediately
    And the Rust binary waits up to 3 seconds for the socket to accept connections

  Scenario: Daemon state enum covers all states
    Then the daemon state enum contains:
      | State            | Description                                           |
      | Starting         | Just spawned, binding socket, setting up watcher       |
      | Evaluating       | nix print-dev-env is running                           |
      | Ready            | Eval succeeded, env files are current                  |
      | BuildFailed      | Nix eval/build failed (daemon stays alive, degraded)   |
      | WatcherDegraded  | File watcher failed, operating on hook-triggered eval   |
      | ConfigError      | Config parse failed, serving cached env                 |
      | ShuttingDown     | Graceful shutdown in progress                          |
    And BuildFailed contains: error string, retry_count, next_retry timestamp
    And ConfigError contains: error string
    And the daemon never crashes due to devshell build failures
    And the daemon never crashes due to config parse failures

  Scenario: Daemon unresponsive — client restarts
    Given a daemon is running for "~/projectA"
    When "xi develop prompt --subshell" tries to connect to the daemon
    And the socket connect times out (>500ms)
    Then the Rust binary attempts to restart the daemon:
      | Attempt | Action                                                |
      | 1       | Send SIGTERM to old PID, wait 200ms, spawn new daemon |
      | 2       | If socket still dead after 3s, try again              |
      | 3       | If still dead, give up                                |
    And if restart succeeds:
      Then the daemon does warm-start from meta.json
      And normal operation resumes
    And if restart fails after 3 attempts:
      Then stderr shows "daemon failed — devshell frozen (using cached env)"
      And the subshell continues with the last-sourced env
      And no further daemon calls until the next directory change

  Scenario: Daemon fatal error
    Given the daemon attempts to start
    When socket bind fails (permission error, path too long)
    Then the daemon logs the fatal error
    And the daemon exits immediately
    And the client detects startup failure (socket never becomes ready)
    And stderr shows "daemon fatal: bind failed: ..."
    And the subshell still spawns with whatever cached env is available
    And no live updates until the daemon issue is resolved

  Scenario: Daemon idle shutdown
    Given the daemon for "~/projectA" is running
    And shell_count drops to 0 (all subshells exited)
    When 60 seconds pass with no new consumers registering
    Then the daemon shuts down gracefully
    And if a nix eval was in progress, it is killed (SIGTERM to nix child process)
    And the socket file is removed
    And the PID file is removed
    And GC roots remain on disk

  Scenario: Daemon receives shutdown request
    Given the daemon is running
    When a client sends a Shutdown request via the socket
    Then the daemon state transitions to "ShuttingDown"
    And the daemon stops accepting new connections
    And the daemon waits for in-progress eval to finish (or kills it after timeout)
    And the daemon removes the socket file
    And the daemon exits

  Scenario: Daemon warm-start from meta.json
    Given a daemon previously ran for "~/projectA" and saved meta.json
    When a new daemon starts for "~/projectA"
    Then the daemon loads meta.json for:
      | Field           | Purpose                                    |
      | packages        | Diff baseline (avoid spurious "all added") |
      | env_hash        | Content-hash dedup on first eval           |
      | store_path      | GC root reference                          |
    And the daemon begins a fresh eval immediately
    And the first eval can detect "unchanged" via content-hash
