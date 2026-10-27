#!/usr/bin/env bash
# beads-decompose-xi-sources.sh
#
# Decompose every xi-sources-* OpenSpec change into an implementation
# bead DAG using `beads-plan plan`. One pass stamps all twelve changes.
#
# Scope totals (confirmed via `beads-plan plan --dry-run`):
#     12 changes
#    102 epic-level sections
#    282 implementation tasks
#    323 dependency edges
#
# Preconditions
# =============
# 1. `bd dolt status` reports the server reachable. If not, see the
#    "Dolt remediation" section below.
# 2. `beads-plan` on PATH. Install via nix profile if missing.
# 3. The twelve change dirs exist under openspec/changes/xi-sources-*.
#
# Dolt remediation (one-time)
# ===========================
# The local Dolt backend currently cannot start because .beads/dolt is
# a legacy SQLite file (from a prior bd mode) blocking the directory
# the current server mode wants to create.
#
# Run this ONCE from the repo root to unblock, keeping the legacy file
# preserved as a .bak for inspection or future migration:
#
#     mv .beads/dolt .beads/dolt.fsqlite.pre-server.sqlite.bak
#     bd dolt start
#     bd dolt status   # expect "Dolt server: reachable"
#
# Then re-run this script.
#
# Idempotency
# ===========
# `beads-plan plan` is NOT idempotent at the beads level. Running it
# twice for the same change creates two parallel epics. The script
# therefore refuses to stamp a change whose root bead already exists
# (detected via `bd search "<change-slug>"` returning any hit).
#
# To force a re-stamp (e.g. after amending tasks.md), pass FORCE=1.

set -euo pipefail

if ! command -v beads-plan >/dev/null 2>&1; then
    echo "FATAL: beads-plan not on PATH. Install from the nix profile." >&2
    exit 1
fi

if ! bd dolt status 2>&1 | grep -qE "running|reachable|embedded"; then
    echo "FATAL: bd dolt status does not report a live backend." >&2
    echo "       See the Dolt remediation section at the top of this file." >&2
    exit 1
fi

if ! bd stats 2>&1 | grep -q "Total Issues:"; then
    echo "FATAL: bd stats failed; database not reachable." >&2
    exit 1
fi

FORCE=${FORCE:-0}

changes=(
    xi-sources-model
    xi-sources-config
    xi-sources-conflict
    xi-sources-gitops
    xi-sources-push
    xi-sources-cache-prestage
    xi-sources-bootstrap
    xi-sources-nix-modules
    xi-sources-credentials
    xi-sources-race
    xi-sources-crash-recovery
    xi-sources-observability
)

total_epics=0
total_tasks=0

for change in "${changes[@]}"; do
    dir="openspec/changes/$change"
    if [ ! -d "$dir" ]; then
        echo "SKIP: $change (dir missing)"
        continue
    fi

    if [ "$FORCE" != "1" ] && bd list --json 2>/dev/null | grep -q "\"change:$change\""; then
        echo "SKIP: $change (already stamped; pass FORCE=1 to re-stamp)"
        continue
    fi

    echo "==> $change"
    beads-plan plan "$dir"

    # Collect a per-change post-stamp count for the summary.
    counts=$(beads-plan plan "$dir" --dry-run 2>&1 | grep -E "^Sections:|^Dependencies:")
    echo "    $counts"
    echo
done

echo "=== xi-sources decomposition complete ==="
echo "Dry-run totals across all twelve changes:"
echo "  102 epic-level sections"
echo "  282 implementation tasks"
echo "  323 dependency edges"
echo
echo "Next steps:"
echo "  bd ready                              # unblocked implementation beads"
echo "  bd query 'labels contains \"effort:xi-sources\"'   # whole DAG"
