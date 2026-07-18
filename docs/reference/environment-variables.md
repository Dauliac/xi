# Environment Variables Reference

## Forwarded Nix variables

These standard Nix variables are forwarded to all underlying `nix` commands
when xi uses environment isolation:

| Variable | Description |
|----------|-------------|
| `NIX_SSHOPTS` | SSH options for remote operations |
| `NIX_CONFIG` | Inline Nix configuration |
| `NIX_REMOTE` | Nix daemon connection URI |
| `NIX_SSL_CERT_FILE` | Custom SSL certificate file |
| `NIX_USER_CONF_FILES` | Extra Nix config files |
| `NIX_SUDOOPTS` | Extra sudo arguments (compat) |
| `NIXOS_INSTALL_BOOTLOADER` | Force bootloader install when `true` |
| `NIXOS_NO_CHECK` | Inhibit NixOS service checks during activation |

## Xi-specific variables

### Flake references

| Variable | Description |
|----------|-------------|
| `XI_FLAKE` | Default flake path for all commands |
| `XI_OS_FLAKE` | Flake for `xi os` (overrides `XI_FLAKE`) |
| `XI_HOME_FLAKE` | Flake for `xi home` (overrides `XI_FLAKE`) |
| `XI_DARWIN_FLAKE` | Flake for `xi darwin` (overrides `XI_FLAKE`) |
| `XI_SYSTEM_FLAKE` | Flake for `xi system` (overrides `XI_FLAKE`) |
| `XI_FILE` | Non-flake Nix file for `xi os switch -f` |
| `XI_ATTRP` | Attribute path for non-flake evaluation |

### Elevation and privilege

| Variable | Description |
|----------|-------------|
| `XI_ELEVATION_STRATEGY` | Override elevation strategy globally |
| `XI_ELEVATION_PROGRAM` | (Deprecated) Old name for `XI_ELEVATION_STRATEGY` |
| `XI_SUDO_ASKPASS` | Program for `SUDO_ASKPASS` during self-elevation |
| `XI_SUDOOPTS` | Extra sudo arguments (takes precedence over `NIX_SUDOOPTS`) |
| `XI_SSHOPTS` | SSH options (takes precedence over `NIX_SSHOPTS`) |
| `XI_BYPASS_ROOT_CHECK` | Skip root privilege check |

### Build behaviour

| Variable | Description |
|----------|-------------|
| `XI_NO_NOM` | Disable nix-output-monitor |
| `XI_SHOW_TRACE` | Enable `--show-trace` |
| `XI_KEEP_GOING` | Enable `--keep-going` |
| `XI_IMPURE` | Enable `--impure` |
| `XI_ACCEPT_FLAKE_CONFIG` | Enable `--accept-flake-config` |
| `XI_OFFLINE` | Enable `--offline` |
| `XI_MAX_JOBS` | Set `--max-jobs` |
| `XI_NO_CHECKS` | Skip startup checks (Nix version, features) |
| `XI_NO_VALIDATE` | Skip pre-activation validation |
| `XI_SHOW_ACTIVATION_LOGS` | Show activation output (`1` to enable) |

### Configuration

| Variable | Description |
|----------|-------------|
| `XI_CONFIG` | Override config file path |
| `XI_LOG` | Tracing filter directive (e.g. `xi=trace`) |
| `XI_NIX_BIN` | Path to real nix binary (set by wrapper) |
| `XI_UNWRAP` | Bypass nix proxy (`1` to enable) |

### Environment preservation

| Variable | Description |
|----------|-------------|
| `XI_PRESERVE_ENV` | `1` to force, `0` to disable env preservation in elevated commands |

### Cache

| Variable | Description |
|----------|-------------|
| `XI_CACHE_URL` | Default push URL |
| `XI_SIGNING_KEY` | Default signing key path |
| `XI_CACHE_ASYNC` | Enable async push |

### Search

| Variable | Description |
|----------|-------------|
| `XI_DEFAULT_SEARCH` | Default search mode: `packages` or `options` |
| `XI_SEARCH_CHANNEL` | Default Nixpkgs channel |
| `XI_SEARCH_JSON` | Truthy value for JSON output |
| `XI_SEARCH_PLATFORM` | Truthy value to show platforms |
| `XI_SEARCH_LIMIT` | Default result limit |
| `XI_OFFLINE_DB` | Colon-separated SPAM database paths |

### GitHub

| Variable | Description |
|----------|-------------|
| `GH_TOKEN` | GitHub token for PR/issue search |
| `XI_GITHUB_TOKEN_FILE` | Override token file path |

### Remote

| Variable | Description |
|----------|-------------|
| `XI_REMOTE_CLEANUP` | Attempt to kill remote processes on interrupt |

### Develop

| Variable | Description |
|----------|-------------|
| `XI_DEVELOP_DAEMON_TIMEOUT` | Socket connection timeout |
| `XI_DEVELOP_EVAL_INTERVAL` | Seconds between re-evaluations |

## Backwards compatibility

| Old variable | New variable | Behaviour |
|-------------|-------------|-----------|
| `FLAKE` | `XI_FLAKE` | Auto-migrated with deprecation warning |
| `XI_ELEVATION_PROGRAM` | `XI_ELEVATION_STRATEGY` | Auto-converted |

## Propagation rules

- All `XI_*` variables are explicitly propagated to subprocess commands during
  environment isolation.
- `NIX_*` variables listed above are forwarded to underlying Nix commands.
- Other environment variables are not passed to elevated or remote commands
  unless `XI_PRESERVE_ENV=1` is set.
