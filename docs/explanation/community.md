# Community and Heritage

Xi stands on the shoulders of the Nix community. This page documents where xi
came from, what patterns it borrowed, and who deserves thanks.

## Lineage: nh

Xi is a fork of **[nh](https://github.com/nix-community/nh)** (nix helper),
originally created by [ViperML](https://github.com/viperML) and later maintained
by [NotAShelf](https://github.com/NotAShelf). Nh pioneered the idea of a
unified, Rust-based CLI for NixOS that calls `nix` directly instead of wrapping
`nixos-rebuild`.

Xi inherits nh's core architecture:

- The crate-per-platform structure (`xi-nixos`, `xi-home`, `xi-darwin`)
- The `Command` builder for subprocess execution with elevation
- The installable resolution system
- The garbage collection reimplementation
- The search backend integration with search.nixos.org
- The NixOS/Home Manager module system We owe an enormous debt to ViperML for
  creating nh and to NotAShelf for growing it into a mature, feature-rich tool
  with search, diffing, remote builds, and the many quality-of-life improvements
  that make it what it is today.

## Tools we run under the hood

### nix-output-monitor (nom)

[nix-output-monitor](https://github.com/maralorn/nix-output-monitor) by
**maralorn** provides the pretty build tree that xi uses by default. Instead of
raw Nix output, you see a live-updating view of which derivations are building,
downloading, or waiting.

Xi pipes `nix build` output through nom when `--no-nom` is not set. The nom
package is injected into PATH by the module system.

### dix

[dix](https://github.com/faukah/dix) by **[faukah](https://github.com/faukah)**
is the fast package diffing library that shows what changed between system
generations. It replaced [nvd](https://sr.ht/~khumba/nvd/) by **khumba** in nh
4.2.0, providing more than twice the performance.

Xi uses dix for both local and remote diffs, comparing store path closures
before and after a switch.

### devour-flake

[devour-flake](https://github.com/srid/devour-flake) by **srid** evaluates all
flake outputs in a single `nix build` invocation. Xi uses it as the default CI
backend in `xi ci`.

### nix-fast-build

[nix-fast-build](https://github.com/Mic92/nix-fast-build) by **Mic92** provides
parallel evaluation with pipelined builds. Xi supports it as an alternative CI
backend, auto-detected when installed.

## Patterns borrowed from the community

### Profile-based GC roots (nix-direnv)

Xi's develop daemon uses `nix --profile <path>` to create GC roots, the same
approach as [nix-direnv](https://github.com/nix-community/nix-direnv). The
profile symlink is a GC root that Nix manages automatically, protecting the
entire devshell closure from garbage collection. This replaced a fragile manual
symlink approach that silently failed.

### Direnv compatibility

Xi's garbage collector understands direnv GC root layouts, including alternative
cache locations (`$XDG_CACHE_DIR/direnv/layouts`). The `--no-direnv` and
`--keep-one` flags exist specifically to cooperate with direnv users.

The develop shell hook system integrates with direnv's model of per-directory
environment activation, extending it with a daemon, live reload, and trust
management.

### SPAM databases (feel-co)

The offline search mode uses [SPAM](https://github.com/feel-co/spam) database
files from the **feel-co** organisation. These are pre-indexed Nixpkgs databases
that allow package search without network access.

### search.nixos.org

Online package and option search is powered by the community's
[search.nixos.org](https://search.nixos.org) Elasticsearch backend. Xi queries
the same API that the web interface uses.

### Test frameworks

Xi supports three community testing frameworks:

- [nix-unit](https://github.com/nix-community/nix-unit) — unit testing for Nix
  expressions
- [nixt](https://github.com/nix-community/nixt) — Nix integration testing
- [namaka](https://github.com/nix-community/namaka) — snapshot testing for Nix

### Formatters

- [alejandra](https://github.com/kamadorueda/alejandra) — opinionated Nix
  formatter by **kamadorueda**
- [treefmt](https://github.com/numtide/treefmt) — multi-language formatter by
  **numtide**
- [nixfmt](https://github.com/NixOS/nixfmt) — the NixOS official formatter

## Thank you

To everyone who has contributed to nh, talked about it, criticised it, or built
the tools it depends on — thank you. Xi would not exist without the Nix
community's collective work.
