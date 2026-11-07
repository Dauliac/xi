# Capability: xi-push-resilient-remote-build

## ADDED Requirements

### Requirement: A remote-build push names its unit and leaves a local receipt

Every `--mode sources` push MUST wrap its remote rebuild in a `systemd-run`
unit given an explicit, predictable name derived from a per-push id, and
MUST write a local receipt naming the target, the unit, and the start time
before the remote invocation begins. The receipt MUST be removed only after
this process (or a later `--reattach`) observes a terminal outcome for that
unit.

#### Scenario: A receipt survives a mid-build disconnect

- **GIVEN** a `--mode sources` push whose SSH session is killed while the
  remote unit is still building
- **WHEN** the local `xi push` process exits
- **THEN** a receipt naming the target, the unit, and the start time
  remains on disk, and the unit is still observably running on the target

### Requirement: `--reattach` recovers a disconnected push's outcome

`xi push --reattach <push-id>` MUST connect to the receipt's target and
report the named unit's current state: running (and stream its journal),
succeeded (and remove the receipt), or failed (and report the failure
without removing the receipt). A unit already unloaded by `--collect` MUST
be resolved by querying journald for that unit name rather than treated as
an error.

#### Scenario: Reattaching to a succeeded unit

- **GIVEN** a receipt whose unit finished successfully while disconnected
- **WHEN** `xi push --reattach <push-id>` runs
- **THEN** it reports success, removes the receipt, and exits zero

#### Scenario: Reattaching to a failed unit

- **GIVEN** a receipt whose unit exited non-zero while disconnected
- **WHEN** `xi push --reattach <push-id>` runs
- **THEN** it reports the failure, including the unit's exit status, and
  does not remove the receipt

#### Scenario: Reattaching to a still-running unit

- **GIVEN** a receipt whose unit is still active
- **WHEN** `xi push --reattach <push-id>` runs
- **THEN** it streams the unit's journal live until a terminal state is
  observed or the operator interrupts, and leaves the receipt in place if
  it does not reach a terminal state this run

#### Scenario: Listing outstanding receipts

- **GIVEN** more than one local receipt exists
- **WHEN** `xi push --reattach` runs with no id
- **THEN** every receipt is listed, newest first, with its target and
  known state

### Requirement: `--notify` fires exactly once per terminal observation

A push started with `--notify <command>` MUST run `<command>` exactly once,
the first time any process (the original push or a later `--reattach`)
observes a terminal state for that push's unit, and MUST NOT run it again
for the same unit.

#### Scenario: Notify does not double-fire under a race

- **GIVEN** the original push process reconnects and observes a terminal
  state at the same moment a concurrent `--reattach` also observes it
- **WHEN** both processes race to act on the terminal state
- **THEN** `--notify`'s command runs exactly once
