#!/usr/bin/env bash
# beads-decompose-manual.sh
#
# Direct tasks.md -> beads decomposition without beads-plan.
# One epic bead per openspec change, one child bead per `- [ ]` task
# line, linked linearly within each `## N. ...` section.
#
# Loses the tier/complexity/parallelism analysis beads-plan adds, but
# produces a correct DAG that `bd ready` can drive.
#
# Idempotent per change: skips if an epic labelled change:<slug>
# already exists.

set -euo pipefail

# Force every bd write to commit immediately. Without this, writes land in
# the Dolt session's working set and vanish when the bd process exits.
# The config-file auto-commit key does not work in bd 1.2.2; env wins.
export BEADS_DOLT_AUTO_COMMIT=on

if ! bd stats >/dev/null 2>&1; then
    echo "FATAL: bd stats failed; database not reachable." >&2
    exit 1
fi

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

bd_q() {
    # bd q: quick-capture that emits ID only and auto-commits (unlike
    # `bd create --json`, which suppresses the commit).
    local title=$1
    local labels=$2
    bd q "$title" --labels "$labels" 2>/dev/null | tail -1
}

decompose_change() {
    local slug=$1
    local tasks="openspec/changes/$slug/tasks.md"

    if [ ! -f "$tasks" ]; then
        echo "SKIP $slug (no tasks.md)"
        return
    fi

    if bd list --json 2>/dev/null | grep -q "\"change:$slug\""; then
        echo "SKIP $slug (already decomposed)"
        return
    fi

    local epic_id
    epic_id=$(bd_q \
        "$slug: implementation epic" \
        "effort:xi-sources,change:$slug,phase:implementation,source:tasks.md")

    if [ -z "$epic_id" ]; then
        echo "FATAL: failed to create epic for $slug" >&2
        return 1
    fi
    echo "==> $slug epic: $epic_id"

    local prev=""
    local section=""
    local task_count=0

    while IFS= read -r line; do
        if [[ "$line" =~ ^##[[:space:]]+([0-9]+)\. ]]; then
            section="${BASH_REMATCH[1]}"
            prev=""
            continue
        fi

        if [[ "$line" == "- [ ] "* ]]; then
            local title="${line#- \[ \] }"
            if [ ${#title} -gt 200 ]; then
                title="${title:0:197}..."
            fi
            local sub
            sub=$(bd_q \
                "$slug: $title" \
                "effort:xi-sources,change:$slug,phase:implementation,section:$section,source:tasks.md")

            if [ -z "$sub" ]; then
                echo "WARN: failed to create subtask ($title) in $slug" >&2
                continue
            fi

            bd dep add "$sub" "$epic_id" --type parent-child >/dev/null 2>&1 \
                || bd dep add "$sub" "$epic_id" >/dev/null 2>&1 \
                || true

            if [ -n "$prev" ]; then
                bd dep add "$sub" "$prev" >/dev/null 2>&1 || true
            fi

            prev="$sub"
            task_count=$((task_count + 1))
        fi
    done < "$tasks"

    echo "    $slug: $task_count implementation beads"
}

total=0
for change in "${changes[@]}"; do
    decompose_change "$change"
    total=$((total + 1))
done

echo
echo "=== decomposition complete ($total changes processed) ==="
echo
echo "Query the DAG:"
echo "  bd query 'labels contains \"effort:xi-sources\"'"
echo "  bd ready"
