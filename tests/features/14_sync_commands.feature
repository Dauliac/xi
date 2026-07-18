Feature: Synchronous commands (exec, enter)
  As a developer using xi develop
  I want synchronous commands to work independently of the daemon
  So that I can use them in CI or one-off scenarios

  Scenario: xi develop enter (sync devshell with nom)
    Given a directory "~/projectA" containing a valid flake.nix
    When the user runs "xi develop" in "~/projectA"
    Then xi ensures flake.lock exists (runs "nix flake lock" if needed)
    And xi builds the devshell with nom (nix build ... | nom)
    And xi evaluates "nix print-dev-env --json"
    And xi shows a package diff (compared to cached meta.json)
    And xi writes env files for all shell types
    And xi creates GC roots
    And xi execs the user's shell with a custom rc that sources the env file
    And the daemon is NOT involved in this flow

  Scenario: xi develop exec -- command
    Given "~/projectA" is trusted
    When the user runs "xi develop exec -- cargo test"
    Then xi checks trust
    And xi evaluates "nix print-dev-env --json" synchronously
    And xi applies env vars to the current process
    And xi builds PATH = nix_paths : original_PATH
    And xi execs "cargo test" via execvp (replaces process)
    And the daemon is NOT involved in this flow

  Scenario: xi develop exec benefits from warm cache
    Given a daemon is running and Ready for "~/projectA"
    When the user runs "xi develop exec -- cargo test"
    Then the nix eval cache is warm (daemon already evaluated)
    And the sync "nix print-dev-env" is nearly instant
    And the command runs with fresh env

  Scenario: xi develop enter with --command flag
    When the user runs "xi develop --command 'cargo build --release'"
    Then xi evaluates the devshell
    And xi runs the command with the devshell env (via sh -c)
    And xi does NOT spawn an interactive shell
    And the command's exit status is propagated
