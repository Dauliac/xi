Feature: Notification system
  As a developer using xi develop
  I want clear, consistent notifications about devshell state
  So that I know what's happening without being overwhelmed

  Scenario: All notifications flow through the daemon
    Given the daemon is running
    Then all devshell state notifications are pushed to the daemon's notification bus
    And the Rust prompt command queries the daemon for notifications
    And the Rust prompt command renders notifications to stderr
    And shell code NEVER generates notifications directly

  Scenario: Global notifications are shown once per terminal
    Given terminal-1 (PID 1001) and terminal-2 (PID 2001) are active
    When the daemon pushes a global notification "devshell updated: + python 3.12"
    Then terminal-1's next prompt drains the notification for PID 1001
    And terminal-2's next prompt drains the notification for PID 2001
    And each terminal shows the notification exactly once
    And subsequent prompts do NOT repeat the notification

  Scenario: Per-instance notifications
    Given terminal-1 is in an untrusted flake
    When terminal-1's prompt hook fires
    Then the "run 'xi develop trust'" message is generated client-side
    And it is NOT sent through the daemon (daemon may not be running)
    And terminal-2 does NOT see terminal-1's per-instance message

  Scenario: Notification types and rendering
    Then the notification system supports these types:
      | Kind    | Icon | Color  | Use case                              |
      | Loading | spin | blue   | Eval in progress                      |
      | Success | check| green  | Devshell ready, updated, recovered    |
      | Info    | dot  | white  | Status messages                       |
      | Warn    | tri  | yellow | Untrusted, degraded, cached mode      |
      | Error   | x    | red    | Eval failed, daemon failed            |
    And each notification is rendered as: "{icon} [xi] {message}"

  Scenario: Notification dedup — same error not repeated
    Given the daemon is in BuildFailed state with error "syntax error at line 5"
    When the daemon retries and fails with the same error
    Then the daemon does NOT push a duplicate notification
    And the error is only shown once per terminal

  Scenario: Notification dedup — different error replaces old
    Given the daemon previously failed with "syntax error at line 5"
    When the daemon retries and fails with "undefined variable 'foo'"
    Then the daemon pushes the new error notification
    And the terminal shows the new error (replacing the old one conceptually)

  Scenario: Exception — client-side fatal notification
    Given the daemon fails to start after 3 restart attempts
    Then the Rust client generates a notification directly (not via daemon)
    And stderr shows "daemon failed — devshell frozen (using cached env)"
    And this is the ONLY case where the client generates notifications directly
