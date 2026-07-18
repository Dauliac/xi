# How to Format, Test, and Inspect Flakes

## Formatting

### Format with the flake formatter

If your flake declares a `formatter` output:

```sh
xi flake fmt
```

Xi builds the formatter with nom and runs it.

### Choose a formatter backend

```sh
xi flake fmt --backend alejandra
xi flake fmt --backend nixfmt
xi flake fmt --backend treefmt
xi flake fmt --backend flake      # use flake's formatter output
xi flake fmt --backend auto       # flake if declared, else nixfmt
```

### Configure the default backend

In `.xi.toml` (project-level):

```toml
[fmt]
backend = "alejandra"
```

Or in `config.toml` (user-level):

```toml
[fmt]
backend = "alejandra"
```

Or via modules:

```nix
{
  programs.xi.fmt.alejandra.enable = true;
  # or
  programs.xi.fmt.treefmt.enable = true;
}
```

Enabling a formatter tool auto-sets the backend and adds the package to PATH.

## Testing

### Run tests

```sh
xi flake test
```

Xi auto-detects available test backends and runs them all.

### Test backends

| Backend    | What it runs                                                                             |
| ---------- | ---------------------------------------------------------------------------------------- |
| `runTests` | Eval-time tests via `lib.runTests` (assertion failures)                                  |
| `checks`   | Build check derivations from `nix flake check`                                           |
| `nix-unit` | [nix-unit](https://github.com/nix-community/nix-unit) — unit testing for Nix expressions |
| `nixt`     | [nixt](https://github.com/nix-community/nixt) — Nix integration testing                  |
| `namaka`   | [namaka](https://github.com/nix-community/namaka) — snapshot testing                     |

### Run specific backends

```sh
xi flake test --backend checks
xi flake test --backend nix-unit --backend namaka
```

### Filter tests by name

```sh
xi flake test --filter "auth*"
```

### Enable test backends via modules

```nix
{
  programs.xi.test.nixUnit.enable = true;
  programs.xi.test.nixt.enable = true;
  programs.xi.test.namaka.enable = true;
}
```

Enabled backends are added to PATH and auto-configured in
`settings.test.backends`.

### Configure test backends in `.xi.toml`

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

# Custom test backends
[[test.custom]]
name = "my-tests"
command = "nix-unit"
args = ["--flake", ".#tests"]
```

### List detected tests

```sh
xi flake test --list
```

Shows test names per backend without running them.

### JSON output for CI

```sh
xi flake test --format json
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

### Interactive snapshot review (namaka)

```sh
xi flake test --review
```

Runs namaka in review mode for interactive acceptance/rejection of snapshot
changes.

### Watch mode

Re-run tests when `.nix`, `.lock`, or `.toml` files change:

```sh
xi flake test --watch
```

Polls every 2 seconds, clears the screen, and re-runs all detected backends.

## Flake doctor

Diagnose issues with your flake:

```sh
xi flake doctor
```

Checks:

- Flake validity and structure
- Input freshness (warns if inputs are older than threshold)
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

## Flake show — output standardization

```sh
xi flake show
```

Xi does not just list raw Nix output. It recognizes implicit output types and
renders them with standardised annotations:

### Recognised output types

| Pattern                                                             | Recognised as         | Rendering                                                 |
| ------------------------------------------------------------------- | --------------------- | --------------------------------------------------------- |
| `packages`, `devShells`, `checks`, `apps`                           | Per-system outputs    | Name, version, `[default]` flag                           |
| `formatter`                                                         | Per-system            | Inline: `formatter :: <tool>`                             |
| `lib`, `*Lib`, `*libs`                                              | Library outputs       | Compact: `lib :: lib (N attrs)` with hint to use `xi lib` |
| `*Module`, `*Modules`, `*modules`                                   | Module outputs        | `:: module` type annotation                               |
| `*Configuration`, `*Configurations`, `*Config`                      | Configuration outputs | `:: configuration` type annotation                        |
| `nixosConfigurations`, `homeConfigurations`, `darwinConfigurations` | System configs        | Discovered tree rendering                                 |
| `overlays`, `templates`                                             | Standard outputs      | Flat listing                                              |
| `debug`, `allSystems`                                               | Internal outputs      | Hidden by default                                         |

### The `[default]` flag

Any output named `default` is flagged with `[default]` in all render paths
(per-system, flat, and discovered trees).

### Test-only categories

When a discovered tree contains only test results (`{expected, expr}` nodes), it
collapses to a summary line: `tests (20 tests)` instead of listing every test
name.

### Show hidden outputs

```sh
xi flake show --all
```

### Raw nix output

```sh
xi flake show --raw
```

### JSON output

```sh
xi flake show --json
```

## Lib outputs

Xi treats `lib` as a first-class output type.

### List lib attributes

```sh
xi lib
```

Recursively lists all attributes in the flake's `lib` output as an indented
tree.

### Deep-evaluate lib

```sh
xi lib --eval
```

Runs `builtins.deepSeq` on the entire lib output, catching type errors, missing
attributes, and infinite recursion that would otherwise only surface at use
time.

```sh
xi lib --eval --show-trace    # detailed error output
```

In CI, lib evaluation runs automatically as a Phase 1 step in `xi flake ci`
(skipped if no lib output exists, disabled with `--no-lib-eval`).
