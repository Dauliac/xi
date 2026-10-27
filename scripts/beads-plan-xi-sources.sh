#!/usr/bin/env bash
# beads-plan-xi-sources.sh
#
# Replayable decomposition of the "xi sources + gitops + bootstrap + push"
# effort into a bead DAG. Run once the local Dolt backend is unstuck:
#
#   bash scripts/beads-plan-xi-sources.sh
#
# Idempotency: this script is NOT idempotent. Each run creates a fresh bead
# set. Run exactly once per effort, or wrap in a guard before re-running.
#
# Convention: every bead is tagged `effort:xi-sources` so a single
# `bd query 'labels contains effort:xi-sources'` scopes the DAG.
# Phase labels follow the `phase:<name>` convention from beads-decompose.
# Change labels follow `change:<slug>` matching OpenSpec change dirs.

set -euo pipefail

labels_root='effort:xi-sources,phase:root'
labels_research='effort:xi-sources,phase:research'
labels_model='effort:xi-sources,phase:design,change:xi-sources-model'
labels_config='effort:xi-sources,phase:design,change:xi-sources-config'
labels_conflict='effort:xi-sources,phase:design,change:xi-sources-conflict'
labels_bootstrap='effort:xi-sources,phase:design,change:xi-sources-bootstrap'
labels_gitops='effort:xi-sources,phase:design,change:xi-sources-gitops'
labels_push='effort:xi-sources,phase:design,change:xi-sources-push'
labels_cache='effort:xi-sources,phase:design,change:xi-sources-cache-prestage'
labels_modules='effort:xi-sources,phase:design,change:xi-sources-nix-modules'
labels_creds='effort:xi-sources,phase:design,change:xi-sources-credentials'
labels_race='effort:xi-sources,phase:design,change:xi-sources-race'
labels_crash='effort:xi-sources,phase:design,change:xi-sources-crash-recovery'
labels_obs='effort:xi-sources,phase:design,change:xi-sources-observability'
labels_synth='effort:xi-sources,phase:synthesis'

# -------- Root --------
root=$(bd q "xi sources + gitops + bootstrap + push: universal pull/push for Nix" \
  --labels "$labels_root" --priority high)

# -------- Research beads (parallel; 5 agents, 5 beads) --------
r_pull=$(bd q    "Research pull-mode GitOps (comin, krops, flake-pull, etc)" \
  --labels "$labels_research,topic:pull-gitops")
r_push=$(bd q    "Research push-mode Nix deploy (deploy-rs, colmena, morph, krops, nixinate, nixops4)" \
  --labels "$labels_research,topic:push-deploy")
r_boot=$(bd q    "Research bootstrap curl|sh (DetSys installer, Lix, nixos-anywhere, rustup, starship)" \
  --labels "$labels_research,topic:bootstrap")
r_layout=$(bd q  "Research state-dir layout (XDG, FHS, systemd StateDirectory, chezmoi, comin)" \
  --labels "$labels_research,topic:state-layout")
r_map=$(bd q     "Map xi codebase insertion points (switch sites, xi-deploy, xi-develop daemon, modules)" \
  --labels "$labels_research,topic:xi-map")

for id in "$r_pull" "$r_push" "$r_boot" "$r_layout" "$r_map"; do
  bd link "$id" "$root"
done

# -------- Research synthesis --------
synth=$(bd q "Synthesize research into a single steal-list and open-questions doc" \
  --labels "$labels_synth" --priority high)
for id in "$r_pull" "$r_push" "$r_boot" "$r_layout" "$r_map"; do
  bd link "$synth" "$id"
done

# -------- Design (OpenSpec) beads: one per change --------
# A: data model is the foundation  -  everything blocks on it.
d_model=$(bd q "OpenSpec: xi-sources-model (sources/state/working-dir + ref lineage)" \
  --labels "$labels_model" --priority high)
bd link "$d_model" "$synth"

# B: config section depends on the data model being named.
d_config=$(bd q "OpenSpec: xi-sources-config (.xi.toml [sources], env, flags, Nix module options)" \
  --labels "$labels_config")
bd link "$d_config" "$d_model"

# C: conflict rules depend on the data model (what are the states).
d_conflict=$(bd q "OpenSpec: xi-sources-conflict (local-wins policy, dirty-tree detection, divergence handling)" \
  --labels "$labels_conflict")
bd link "$d_conflict" "$d_model"

# D: gitops daemon depends on model + config + conflict (it enforces conflict on every tick).
d_gitops=$(bd q "OpenSpec: xi-sources-gitops (pull daemon, interval, trust, rollback)" \
  --labels "$labels_gitops")
bd link "$d_gitops" "$d_model"
bd link "$d_gitops" "$d_config"
bd link "$d_gitops" "$d_conflict"

# E: push depends on model + config (where does the pushed state land, which backend).
d_push=$(bd q "OpenSpec: xi-sources-push (push to remote hosts via deploy-rs/colmena/builtin; closure vs source)" \
  --labels "$labels_push")
bd link "$d_push" "$d_model"
bd link "$d_push" "$d_config"

# F: bootstrap depends on model + config + cache-prestage (chicken-and-egg).
# cache-prestage is noted as deferred mechanism but documented now.
d_cache=$(bd q "OpenSpec: xi-sources-cache-prestage (configure substituters from unbuilt module BEFORE first build)" \
  --labels "$labels_cache" --priority medium)
bd link "$d_cache" "$d_model"
bd link "$d_cache" "$d_config"

d_boot=$(bd q "OpenSpec: xi-sources-bootstrap (curl|sh plus xi system switch URL, incl. nix-install delegation)" \
  --labels "$labels_bootstrap")
bd link "$d_boot" "$d_model"
bd link "$d_boot" "$d_config"
bd link "$d_boot" "$d_cache"

# G: Nix module option tree. Blocks every other spec that references module keys.
# Added in the second-pass design; depends on model + config so option names
# can cross-reference them cleanly.
d_modules=$(bd q "OpenSpec: xi-sources-nix-modules (full xi.sources.* option tree, three-head parity)" \
  --labels "$labels_modules" --priority high)
bd link "$d_modules" "$d_model"
bd link "$d_modules" "$d_config"

# H: Credentials. Needed by every actor that touches a remote.
d_creds=$(bd q "OpenSpec: xi-sources-credentials (ssh/https/oauth/anonymous fallthrough, redacted at rest)" \
  --labels "$labels_creds")
bd link "$d_creds" "$d_config"
bd link "$d_creds" "$d_modules"

# I: Race model. Needs model + conflict (actors) + nix-modules (option visibility).
d_race=$(bd q "OpenSpec: xi-sources-race (advisory lock, pause/resume, priority queue)" \
  --labels "$labels_race")
bd link "$d_race" "$d_model"
bd link "$d_race" "$d_conflict"
bd link "$d_race" "$d_gitops"
bd link "$d_race" "$d_push"

# J: Crash recovery. Needs race (lock) and model (state dir shape).
d_crash=$(bd q "OpenSpec: xi-sources-crash-recovery (transaction wrapper, shadow write, xi sources repair)" \
  --labels "$labels_crash")
bd link "$d_crash" "$d_model"
bd link "$d_crash" "$d_race"
bd link "$d_crash" "$d_gitops"
bd link "$d_crash" "$d_boot"
bd link "$d_crash" "$d_cache"

# K: Observability. Pulls in credentials (redaction) and gitops (where ticks happen).
d_obs=$(bd q "OpenSpec: xi-sources-observability (tracing, Prometheus, /healthz, OTLP, redaction)" \
  --labels "$labels_obs")
bd link "$d_obs" "$d_gitops"
bd link "$d_obs" "$d_creds"
bd link "$d_obs" "$d_modules"

# -------- Validation --------
val=$(bd q "openspec validate for every xi-sources-* change (strict)" \
  --labels "effort:xi-sources,phase:validate")
for id in "$d_model" "$d_config" "$d_conflict" "$d_gitops" "$d_push" \
          "$d_cache" "$d_boot" "$d_modules" "$d_creds" "$d_race" \
          "$d_crash" "$d_obs"; do
  bd link "$val" "$id"
done

# -------- Human review gate --------
review=$(bd q "Operator review of 12 OpenSpec changes + resolve open questions" \
  --labels "effort:xi-sources,phase:review,blocking:human")
bd link "$review" "$val"

echo
echo "=== xi-sources bead DAG created ==="
echo "root:         $root"
echo "research:     $r_pull $r_push $r_boot $r_layout $r_map"
echo "synthesis:    $synth"
echo "design:       $d_model $d_config $d_conflict $d_gitops $d_push $d_cache $d_boot"
echo "              $d_modules $d_creds $d_race $d_crash $d_obs"
echo "validate:     $val"
echo "review gate:  $review"
echo
echo "Next: bd ready   # should surface the 5 research beads"
