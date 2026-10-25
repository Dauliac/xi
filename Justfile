#! /usr/bin/env -S just --justfile

# Recipes to check correctness
check: cargo-check clippy-check fmt-check taplo-check deno-check
cargo-check:
    RUSTFLAGS="-Dwarnings" cargo check

clippy-check:
    cargo clippy -- -W clippy::pedantic -W clippy::correctness -W clippy::suspicious -W clippy::cargo

fmt-check:
    cargo fmt --check

taplo-check:
    taplo fmt --check

deno-check:
    deno fmt --check

# Automatic fixup recipes
fix: cargo-fix clippy-fix fmt-fix taplo-fmt deno-fmt
cargo-fix:
    cargo fix --allow-dirty

clippy-fix:
    cargo clippy --fix --allow-dirty -- -W clippy::pedantic -W clippy::correctness -W clippy::suspicious -W clippy::cargo

fmt-fix:
    cargo fmt

taplo-fmt:
    taplo fmt

deno-fmt:
    deno fmt .

test:
    cargo nextest run

# Regenerate .github/workflows/*.yml from the Nix definitions in
# nix/github-actions.nix. Run after editing that module; commit the
# resulting YAML. Each generated file has a "Do not edit directly" banner.
workflows-sync:
    #!/usr/bin/env bash
    set -euo pipefail
    out=$(nix build --no-link --print-out-paths .#legacyPackages.x86_64-linux.github-workflows)
    for f in "$out"/*.yml; do
      install -m 0644 "$f" ".github/workflows/$(basename "$f")"
    done
    echo "Synced $(ls "$out"/*.yml | wc -l) workflow(s) into .github/workflows/"
