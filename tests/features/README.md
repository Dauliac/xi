# BDD Feature Specifications

This directory contains Gherkin-format BDD scenarios that serve as the **source
of truth** for `xi develop` async devshell behavior.

## File Index

| File                                  | Feature                           | Testable in Rust?                         |
| ------------------------------------- | --------------------------------- | ----------------------------------------- |
| `01_first_entry_trusted.feature`      | First entry into trusted flake    | Integration (daemon + shell stub)         |
| `02_first_entry_untrusted.feature`    | Entry into untrusted flake        | Unit (trust check)                        |
| `03_live_update_package.feature`      | Package add/remove/update         | Integration (daemon eval + notifications) |
| `04_live_update_hook.feature`         | shellHook changes                 | Integration (daemon eval + hook file)     |
| `05_live_update_noop.feature`         | No-op on non-nix edits            | Unit (content-hash dedup)                 |
| `06_leaving_flake.feature`            | cd out of flake                   | Integration (prompt command + daemon)     |
| `07_nested_flakes.feature`            | Monorepo nesting                  | Integration (multi-daemon + shell chain)  |
| `08_nested_parent_env_change.feature` | Parent env change while nested    | Integration                               |
| `09_trust_untrust.feature`            | Trust/untrust lifecycle           | Unit (trust module)                       |
| `10_eval_failure_recovery.feature`    | Eval failure + backoff + recovery | Unit (daemon state machine)               |
| `11_daemon_lifecycle.feature`         | Daemon start/stop/restart         | Integration (lifecycle module)            |
| `12_multi_terminal.feature`           | Multiple terminals same flake     | Unit (notification queue)                 |
| `13_user_exit_propagation.feature`    | Exit/Ctrl+D behavior              | Integration (shell + trap)                |
| `14_sync_commands.feature`            | exec, enter (sync)                | Integration (no daemon)                   |
| `15_version_upgrade.feature`          | Binary version mismatch           | Unit (lifecycle version check)            |
| `16_debug_logging.feature`            | XI_LOG=debug tracing              | Manual verification                       |
| `17_notifications.feature`            | Notification bus                  | Unit (notification queue + rendering)     |
| `18_file_watcher.feature`             | File watcher patterns             | Unit (pattern matching)                   |
| `19_shell_uniformity.feature`         | bash/zsh/fish parity              | Integration (activation scripts)          |
| `20_daemon_protocol.feature`          | Socket protocol                   | Unit (serialization roundtrip)            |

## Principles

1. **These files are the spec.** When behavior changes, update the feature file
   FIRST.
2. **One feature per file.** If a new use case is spotted, create a new
   `.feature` file.
3. **Bugs become scenarios.** Every bug fix adds a regression scenario to the
   relevant feature.
4. **Rust tests reference features.** Test functions include
   `// BDD: tests/features/XX_name.feature#scenario-name`.
