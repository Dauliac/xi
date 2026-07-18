Feature: File watcher behavior
  As a developer using xi develop
  I want the daemon to watch relevant files and react to changes
  So that my devshell updates automatically when I edit nix files

  Background:
    Given the daemon is running for "~/projectA"
    And "~/projectA" is a git repository

  Scenario: Default watch patterns
    Then the watcher monitors files matching:
      | Pattern    | Example files              |
      | *.nix      | flake.nix, shell.nix, modules/foo.nix |
      | flake.lock | flake.lock                 |
    And the watcher discovers directories by scanning the git index
    And the watcher watches each unique parent directory (NonRecursive)
    And the watcher also watches .git/index (for git add/rm)

  Scenario: Extra watch patterns from config
    Given ~/.config/xi/config.toml contains:
      """
      [develop]
      watch_extra = ["*.yaml", "version.txt", "Cargo.lock"]
      """
    Then the watcher also monitors files matching:
      | Pattern      | Example files         |
      | *.yaml       | config.yaml           |
      | version.txt  | version.txt           |
      | Cargo.lock   | Cargo.lock            |

  Scenario: File change triggers re-evaluation
    When a watched file is modified
    Then the watcher sends a FileChangeEvent
    And the daemon sets change_pending=true
    And the daemon clears any error state (resets backoff)
    And the daemon re-evaluates on the next cycle (respecting rate limit)

  Scenario: Rate limiting prevents eval storm
    Given the daemon's eval_interval is 5 seconds
    When multiple file changes happen within 5 seconds
    Then the daemon only evaluates once (after the rate limit expires)
    And rapid file saves (e.g., auto-save) do not cause eval storms

  Scenario: Watcher failure degrades gracefully
    When the git2 or notify watcher setup fails
    Then the daemon logs a warning
    And the daemon state includes WatcherDegraded
    And the daemon falls back to hook-triggered eval (re-evaluates when shells connect)
    And the daemon does NOT crash
