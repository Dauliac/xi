Feature: First entry into an untrusted flake directory
  As a developer using xi develop
  I want to be warned when entering an untrusted flake
  So that I can decide whether to trust it before activating

  Background:
    Given a parent shell with eval "$(xi develop activate zsh)" loaded

  Scenario: Entry into an untrusted flake
    Given a directory "~/projectA" containing a valid flake.nix
    And "~/projectA" is NOT trusted
    When the user runs "cd ~/projectA"
    And the parent prompt hook fires
    And "xi develop prompt" runs
    Then "xi develop prompt" detects flake.nix at "~/projectA"
    And "xi develop prompt" checks the trust status
    And the trust check fails
    And "xi develop prompt" outputs nothing to stdout (no subshell spawned)
    And stderr shows a per-instance warning: "run 'xi develop trust' to activate devshell"
    And no daemon is started

  Scenario: Trust notification is per-instance
    Given terminal-1 and terminal-2 are both in untrusted "~/projectA"
    When each terminal's prompt hook fires
    Then each terminal independently shows the trust warning on stderr
    And the warning is generated client-side (not from a daemon)
    And no daemon is started for either terminal
