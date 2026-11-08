# Capability: xi-repl

## ADDED Requirements

### Requirement: `xi repl` is an interactive, daemon-backed Nix evaluation session

`xi repl` MUST provide a persistent interactive session (via `reedline`)
distinct from `nix-command`'s existing `CommandKind::Repl` passthrough. It MUST
evaluate expressions through the `xi-develop` eval daemon's `ExprEval` request
rather than cold-starting a fresh evaluator per expression, reusing the daemon's
existing warm-eval state for the current flake.

#### Scenario: Session reuses warm eval state

- **GIVEN** an `xi-develop` daemon already running for the current flake with a
  fresh (not stale) `eval_input_hash`
- **WHEN** `xi repl` starts and evaluates its first expression
- **THEN** it does not re-run a full flake evaluation from scratch; it reuses
  the daemon's already-resolved state

#### Scenario: No daemon running, no `--no-daemon` flag

- **GIVEN** no `xi-develop` daemon is currently running for the current flake
- **WHEN** `xi repl` starts
- **THEN** it auto-spawns a session-scoped daemon rather than failing or
  silently falling back to a degraded mode

#### Scenario: `--no-daemon` requests the degraded mode explicitly

- **GIVEN** `xi repl --no-daemon` is invoked
- **WHEN** the session starts
- **THEN** it evaluates each expression as an always-cold `nix eval` invocation,
  with coloring and completion still available, and does not spawn a daemon

### Requirement: Shallow completion never forces derivation realisation

Attribute-path completion (`.`/Tab) MUST list candidate attribute names via
`ExprEval { shallow: true }`, which MUST NOT force evaluation deep enough to
realise (build) any candidate derivation. Only the candidates actually rendered
in the completion dropdown MAY be further enriched with
`meta.description`/version.

#### Scenario: Listing attribute names does not build anything

- **GIVEN** the user types `pkgs.` and presses Tab against nixpkgs
- **WHEN** the completion menu populates with candidate attribute names
- **THEN** no candidate's derivation is realised (built) as a side effect of
  populating the list

#### Scenario: Only visible candidates are enriched

- **GIVEN** a completion menu showing N visible candidates out of a much larger
  candidate set
- **WHEN** the preview pane renders descriptions
- **THEN** `meta.description`/version enrichment is requested only for the N
  visible candidates, not the full candidate set

### Requirement: Derivation-valued results render as a structured card

Evaluating an expression whose value is a derivation MUST render a colorized
card showing at least name, version, and description (when present), using the
shared renderer also used by `xi flake show` and `xi search`. It MUST NOT render
the raw, unparsed `«derivation /nix/store/...»` string that plain `nix repl`
produces.

#### Scenario: Evaluating a package shows a structured card

- **GIVEN** the expression `pkgs.ripgrep` is evaluated
- **WHEN** the result is displayed
- **THEN** it shows a colorized card with the package's name, version, and
  description, not an opaque derivation path string

### Requirement: The value renderer is a single shared implementation

The name/version/description parsing and coloring logic used by `xi
repl`,
`xi flake show`, and `xi search` MUST be one shared implementation (e.g. in
`xi-core`), not independently duplicated per command. Extracting it out of
`xi-flake`'s and `xi-search`'s existing one-shot implementations MUST NOT change
either command's existing output.

#### Scenario: `xi flake show` output is unchanged after extraction

- **GIVEN** `xi flake show`'s existing test fixtures
- **WHEN** `show.rs` is refactored to call the shared renderer instead of its
  own private parsing
- **THEN** the command's output on those fixtures is byte-identical to before
  the refactor

#### Scenario: `xi search`'s rendering is unchanged after extraction

- **GIVEN** `xi search`'s existing test fixtures
- **WHEN** `render/packages.rs` is refactored to call the shared renderer
- **THEN** the command's output on those fixtures is byte-identical to before
  the refactor

### Requirement: Unsupported expressions fail explicitly with an escape hatch

An expression the daemon's `ExprEval` path cannot evaluate (e.g. requires full
import-from-derivation the daemon's restricted path does not support, or no
daemon is reachable and auto-spawn failed) MUST surface a named error
identifying why, and MUST offer a `:raw` command that re-executes the expression
against a real `nix repl` subprocess. It MUST NOT hang the session or crash it.

#### Scenario: Daemon-unsupported expression surfaces a named error

- **GIVEN** an expression that the daemon's `ExprEval` handler rejects
- **WHEN** it is evaluated in `xi repl`
- **THEN** the session prints a named error explaining why, remains responsive,
  and offers `:raw` as the next step

#### Scenario: `:raw` falls back to real `nix repl`

- **GIVEN** the named-error state from the previous scenario
- **WHEN** the user runs `:raw`
- **THEN** the same expression is evaluated via a real `nix repl` subprocess and
  its result is shown

### Requirement: `:doc` surfaces package metadata, and is explicit about what it does not cover

`:doc <expr>` MUST surface `meta.description`/`meta.longDescription` for an
expression that evaluates to a package. For an expression that is not a package
(e.g. a `lib` function or builtin), it MUST say explicitly that documentation
lookup is not available for that kind of value in this version, rather than
returning an empty or misleading result.

#### Scenario: `:doc` on a package shows its metadata

- **GIVEN** `pkgs.ripgrep` is the last-evaluated expression
- **WHEN** `:doc` is run
- **THEN** it shows `meta.description` and `meta.longDescription` (when present)
  for that package

#### Scenario: `:doc` on a non-package value says so explicitly

- **GIVEN** a `lib` function is the last-evaluated expression
- **WHEN** `:doc` is run
- **THEN** it states that documentation lookup for this kind of value is not
  supported in this version, rather than printing nothing or an unrelated result
