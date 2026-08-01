# How to format, test, and inspect flakes

## Format the tree

If your flake declares a `formatter` output:

```sh
xi fmt
```

Xi builds the formatter and runs it against the tree with pretty output.

### Pick a different formatter

```sh
xi fmt --backend alejandra
xi fmt --backend nixfmt
xi fmt --backend treefmt
xi fmt --backend flake      # use the flake's formatter output
xi fmt --backend auto       # flake if declared, else nixfmt
```

### Persist your choice

Project-wide in `.xi.toml`:

```toml
[fmt]
backend = "alejandra"
```

Or user-wide in `config.toml`:

```toml
[fmt]
backend = "alejandra"
```

Or through the xi module system:

```nix
{
  programs.xi.fmt.alejandra.enable = true;
  # or
  programs.xi.fmt.treefmt.enable = true;
}
```

Enabling a formatter through the module auto-sets the backend and adds the tool
to `PATH`.

## Run tests

```sh
xi test
```

Xi auto-detects available test backends and runs them all in one pass.

### Available backends

| Backend    | What it runs                                                                             |
| ---------- | ---------------------------------------------------------------------------------------- |
| `runTests` | Eval-time tests via `lib.runTests`                                                       |
| `checks`   | Check derivations produced by `nix flake check`                                          |
| `nix-unit` | [nix-unit](https://github.com/nix-community/nix-unit) — unit testing for Nix expressions |
| `nixt`     | [nixt](https://github.com/nix-community/nixt) — integration testing                      |
| `namaka`   | [namaka](https://github.com/nix-community/namaka) — snapshot testing                     |

### Restrict to specific backends

```sh
xi test --backend checks
xi test --backend nix-unit --backend namaka
```

### Filter by test name

```sh
xi test --filter "auth*"
```

### Enable backends via modules

```nix
{
  programs.xi.test.nixUnit.enable = true;
  programs.xi.test.nixt.enable = true;
  programs.xi.test.namaka.enable = true;
}
```

Enabled backends are added to `PATH` and auto-configured in
`settings.test.backends`.

### Configure in `.xi.toml`

```toml
[test]
backends = ["runTests", "checks", "nix-unit"]

# Attribute path for eval-time tests
runTests.attr = "tests"

# Glob filter on check names
checks.filter = "test-*"

# Directory for standalone tools
nix-unit.test-dir = "tests/"
nixt.test-dir = "tests/"

# Custom backend
[[test.custom]]
name = "my-tests"
command = "nix-unit"
args = ["--flake", ".#tests"]
```

### List detected tests

```sh
xi test --list
```

Shows test names per backend without running them.

### Emit JSON for CI

```sh
xi test --format json
```

```json
{
  "passed": 19, "failed": 1, "errors": 0, "total": 20,
  "duration_secs": 6.7,
  "backends": [
    { "backend": "runTests", "duration_secs": 0.2, "tests": [...] }
  ]
}
```

### Review snapshot changes (namaka)

```sh
xi test --review
```

Runs namaka in review mode for interactive accept/reject of snapshot changes.

### Watch mode

```sh
xi test --watch
```

Re-runs the detected backends when `.nix`, `.lock`, or `.toml` files change.
Clears the screen between runs.

## Diagnose the flake

```sh
xi doctor
```

Reports:

- Flake input freshness (warns when any input is older than the configured age)
- Nixpkgs source verification (warns on unofficial forks)
- Branch validation against allowed branches
- Missing or misconfigured outputs

Configure thresholds in `.xi.toml`:

```toml
[doctor]
max-input-age-days = 30
require-official-nixpkgs = true
supported-branches = ["nixos-unstable", "master"]
```

## Inspect flake outputs

```sh
xi show
```

Xi renders outputs by category with type annotations, hiding empty per-system
categories (`legacyPackages`, empty `checks`, etc.) unless you ask for them.

### Recognised categories

| Pattern                                                             | Rendered as                                         |
| ------------------------------------------------------------------- | --------------------------------------------------- |
| `packages`, `devShells`, `checks`, `apps`                           | Per-system tables                                   |
| `formatter`                                                         | Inline: `formatter :: <tool>`                       |
| `lib`, `*Lib`, `*libs`                                              | Compact `lib (N attrs)` with a hint to use `xi lib` |
| `*Module`, `*Modules`                                               | `:: module`                                         |
| `*Configuration`, `*Configurations`, `*Config`                      | `:: configuration`                                  |
| `nixosConfigurations`, `homeConfigurations`, `darwinConfigurations` | Discovered tree                                     |
| `overlays`, `templates`                                             | Flat listing                                        |
| `debug`, `allSystems`                                               | Hidden by default                                   |

Any output named `default` is tagged `[default]` in every render path.

### Show hidden outputs

```sh
xi show --all
```

### Raw nix output

```sh
xi show --raw
```

### JSON output

```sh
xi show --json
```

## Inspect lib outputs

```sh
xi lib
```

Recursively lists the flake's `lib` output as an indented tree.

### Deep-evaluate lib

```sh
xi lib --eval
```

Runs `builtins.deepSeq` on the entire `lib`, catching type errors, missing
attributes, and infinite recursion that would otherwise only surface at use
time.

```sh
xi lib --eval --show-trace    # detailed error output
```

In CI, `xi ci` runs this automatically. Disable with `--no-lib-eval`.

## See also

- [CLI Reference: `xi fmt`, `xi test`, `xi show`, `xi lib`, `xi doctor`](../reference/cli.md)
- [Configuration Reference: `[fmt]`, `[test]`, `[doctor]`](../reference/configuration.md)
