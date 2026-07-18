<!-- markdownlint-disable MD033 MD041 -->
<div align="center">
  <h1 id="header">xi</h1>
  <a alt="CI" href="https://github.com/Dauliac/xi/actions">
    <img
      src="https://github.com/Dauliac/xi/actions/workflows/build.yaml/badge.svg"
      alt="Build Status"
    />
  </a>
  <a alt="Deps" href="https://deps.rs/repo/github/Dauliac/xi">
    <img
      src="https://deps.rs/repo/github/Dauliac/xi/status.svg"
      alt="Dependency Status"
    />
  </a>
  <a alt="License" href="https://github.com/Dauliac/xi/blob/master/LICENSE">
    <img
      src="https://img.shields.io/github/license/Dauliac/xi?label=License"
      alt="License"
    />
  </a>
</div>

---

**xi** (/ksaɪ/) — the Greek letter, nix reversed without the n, and a two-character
CLI. A modern, unified tool for the Nix ecosystem. It reimplements and extends
the interfaces of `nixos-rebuild`, `home-manager`, `darwin-rebuild`,
`nix search`, and `nix-collect-garbage` into a single, cohesive tool with pretty
output, fast diffing, and daemon-driven development shells.

Xi is a fork of [nh](https://github.com/nix-community/nh) by
[ViperML](https://github.com/viperML) and
[NotAShelf](https://github.com/NotAShelf). See
[Community and Heritage](explanation/community.md) for full acknowledgments.

```sh
nix shell github:Dauliac/xi  # try it now
```

## Documentation

This documentation follows the [Diataxis](https://diataxis.fr/) framework. Pick
the section that matches what you need right now.

### Tutorials — learn by doing

Start here if you are new to xi.

| Guide                                             | What you will build                               |
| ------------------------------------------------- | ------------------------------------------------- |
| [Getting Started](tutorials/getting-started.md)   | Install xi and switch your first NixOS generation |
| [Develop Workflow](tutorials/develop-workflow.md) | Set up daemon-driven devshells with live reload   |

### How-to Guides — achieve a specific goal

Jump to the guide that matches your task.

| Guide                                                                   | Goal                                                           |
| ----------------------------------------------------------------------- | -------------------------------------------------------------- |
| [NixOS](guides/nixos.md)                                                | Switch, boot, test, build, rollback NixOS systems              |
| [Home Manager](guides/home-manager.md)                                  | Switch and build Home Manager configurations                   |
| [Darwin](guides/darwin.md)                                              | Switch and build nix-darwin configurations                     |
| [Search](guides/search.md)                                              | Search packages, options, PRs, and issues                      |
| [Clean](guides/clean.md)                                                | Garbage-collect the Nix store with fine-grained control        |
| [Remote Build](guides/remote-build.md)                                  | Build on remote hosts and deploy over SSH                      |
| [Module Setup](guides/module-setup.md)                                  | Configure NixOS, Home Manager, or flake-parts modules          |
| [Nix Proxy](guides/nix-proxy.md)                                        | Use xi as a transparent replacement for the nix CLI            |
| [Binary Cache](guides/binary-cache.md)                                  | Push build results to S3, SSH, or Cachix                       |
| [CI and Materialization](guides/ci.md)                                  | Multi-phase CI pipeline, materialization, and freshness checks |
| [Formatting, Testing, and Inspection](guides/formatting-and-testing.md) | Format code, run tests, inspect outputs, evaluate libs         |

### Reference — look up facts

Precise, complete specifications for every knob xi exposes.

| Document                                                    | Covers                                       |
| ----------------------------------------------------------- | -------------------------------------------- |
| [CLI Reference](reference/cli.md)                           | Every command, subcommand, and flag          |
| [Environment Variables](reference/environment-variables.md) | All `XI_*` and forwarded `NIX_*` variables   |
| [Configuration](reference/configuration.md)                 | `config.toml` and `.xi.toml` format          |
| [Module Options](reference/module-options.md)               | NixOS, Home Manager, and flake-parts options |

### Explanation — understand the design

Read these when you want to understand _why_ xi works the way it does.

| Document                                           | Topic                                                   |
| -------------------------------------------------- | ------------------------------------------------------- |
| [Architecture](explanation/architecture.md)        | Design philosophy and how xi works under the hood       |
| [Community and Heritage](explanation/community.md) | nh lineage, borrowed patterns, and acknowledgments      |
| [The Xi Specification](explanation/xi-spec.md)     | Conventions and guarantees: naming, cascades, elevation |
| [The Develop Model](explanation/develop-model.md)  | Daemon, shell integration, trust, and live reload       |
